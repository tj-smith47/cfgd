use crate::PathDisplayExt;
use crate::config::LOCAL_LAYER;
use crate::providers::{FileAction, PackageAction, SecretAction};
use crate::to_posix_string;

use super::types::{
    Action, EnvAction, ManagerAction, ModuleAction, ModuleActionKind, OwnerGroup, ScriptAction,
    ScriptPhase, SystemAction,
};

/// Resource id of the live-session env refresh. The planner and
/// `apply_env_action` must both emit it verbatim: it is the only env surface
/// with no path to key on, so a divergence between the two makes the applied
/// result unmatchable against the action that planned it.
pub(super) const LIVE_SESSION_RESOURCE_ID: &str = "env:session:refresh";

/// The ONE composition of a system setting's `<configurator>.<key>` identity.
///
/// Three surfaces write this string and match it against each other:
/// [`format_action_description`] mints the `managed_resources` id and the
/// journal `resource_id`, `cli::live_drift` mints the `drift_events` id that
/// `resolve_drift` must find, and `compliance::collect_system_checks` mints the
/// check key `compliance diff` pairs two snapshots on. A byte of divergence
/// between any two of them means drift that is recorded but never resolved, so
/// they derive it here rather than each holding its own `format!`.
///
/// The debug assertion is the structural half of the same guarantee: a
/// [`crate::providers::SystemDrift::key`] that opens with its own
/// configurator's name doubles the name into all three, and because two of them
/// are persisted, undoing that later costs a state migration. Debug-only
/// because the string still composes correctly for an id that carries it — the
/// result is ugly and expensive, not wrong — and a release build must not
/// panic mid-apply over a naming defect.
pub fn system_resource_key(configurator: &str, key: &str) -> String {
    debug_assert_system_key_undoubled(configurator, key);
    format!("{configurator}.{key}")
}

/// The diagnostic for a drift key that repeats its own configurator's name, or
/// `None` when the key is well-formed.
///
/// The ONE statement of the rule, so the two enforcement sites cannot drift
/// apart in what they detect or in what they say: this crate asserts it in
/// debug builds through `debug_assert_system_key_undoubled`, and each
/// configurator's diff test asserts it unconditionally against its own fixture
/// (`cfgd::system::assert_keys_undoubled`).
pub fn system_key_doubling_error(configurator: &str, key: &str) -> Option<String> {
    key.starts_with(&format!("{configurator}.")).then(|| {
        format!(
            "{configurator}: drift key `{key}` repeats the configurator name; \
             `system:{configurator}.<key>` is composed around it"
        )
    })
}

/// The diagnostic for a pre-skip reason that repeats a noun its own action's
/// subject already carries, or `None` when the two halves say different things.
///
/// The ONE statement of the rule for the withheld row's two slots, and the
/// third member of the family `system_key_doubling_error` and
/// `note_tag_doubling_error` already hold: a row composes as
/// `<subject> — <reason>`, so a subject naming the very thing the reason says
/// is missing prints one noun twice on one line —
/// `publish 3 vars to the session manager — no session manager`. The subject
/// names what the work is FOR, the reason names what the host does not have.
///
/// Judged on the reason's own noun, which is the reason minus the `no ` a
/// wording of absence opens on: [`crate::NO_SESSION_MANAGER`] stays the one
/// wording of that absence, read by the plan, the apply's skip detail and
/// `status`'s session row alike, so the doubling is fixed on the subject side.
pub fn pre_skip_doubling_error(subject: &str, reason: &str) -> Option<String> {
    let noun = reason.strip_prefix("no ").unwrap_or(reason);
    subject.contains(noun).then(|| {
        format!(
            "pre-skip row `{subject} — {reason}` repeats `{noun}`;              the subject names the work and the reason names what is missing"
        )
    })
}

/// Debug-only guard that a pre-skip reason does not double its own subject,
/// returning the reason so every arm of [`Action::pre_skip_reason`] is checked
/// by the shape of how it answers rather than by a test remembering to.
///
/// Debug-only for the same reason its drift-key sibling is: the row still
/// composes, it just says one noun twice, and a release build must not panic
/// mid-plan over a wording defect.
pub(crate) fn debug_checked_pre_skip_reason(action: &Action, reason: &'static str) -> &'static str {
    if cfg!(debug_assertions)
        && let Some(message) =
            pre_skip_doubling_error(&action_display_subject(action).to_string(), reason)
    {
        debug_assert!(false, "{message}");
    }
    reason
}

/// Debug-only guard that a configurator's drift key does not repeat its name.
///
/// Called by [`system_resource_key`] and from the planner, which is where a
/// configurator's `diff` output first meets its name and so catches every
/// configurator on every planning run rather than only the ones whose keys a
/// test pins.
pub(crate) fn debug_assert_system_key_undoubled(configurator: &str, key: &str) {
    if cfg!(debug_assertions)
        && let Some(message) = system_key_doubling_error(configurator, key)
    {
        debug_assert!(false, "{message}");
    }
}

/// Append source provenance suffix for non-local origins.
pub(super) fn provenance_suffix(origin: &str) -> String {
    if origin.is_empty() || origin == LOCAL_LAYER {
        String::new()
    } else {
        format!(" <- {origin}")
    }
}

/// Format a canonical description of an action.
///
/// Used as the SQLite `managed_resource` resource_id, the
/// `ActionResult.description` JSON field, AND the user-facing apply-error
/// printer line. Embedded paths are always folded to POSIX form via
/// `to_posix_string` so the same logical resource carries the same key on
/// every OS — drift correlation, JSON wire form, and human display all
/// agree.
pub fn format_action_description(action: &Action) -> String {
    let path_str = to_posix_string;
    match action {
        Action::File(fa) => match fa {
            FileAction::Create { target, .. } => format!("file:create:{}", path_str(target)),
            FileAction::Update { target, .. } => format!("file:update:{}", path_str(target)),
            FileAction::Delete { target, .. } => format!("file:delete:{}", path_str(target)),
            FileAction::SetPermissions { target, mode, .. } => {
                format!("file:chmod:{:#o}:{}", mode, path_str(target))
            }
            FileAction::Skip { target, .. } => format!("file:skip:{}", path_str(target)),
        },
        Action::Package(pa) => match pa {
            PackageAction::Install {
                manager, packages, ..
            } => format!("package:{}:install:{}", manager, packages.join(",")),
            PackageAction::Uninstall {
                manager, packages, ..
            } => format!("package:{}:uninstall:{}", manager, packages.join(",")),
            PackageAction::Skip { manager, .. } => format!("package:{}:skip", manager),
        },
        Action::Secret(sa) => match sa {
            SecretAction::Decrypt {
                target, backend, ..
            } => format!("secret:decrypt:{}:{}", backend, path_str(target)),
            SecretAction::Resolve {
                provider,
                reference,
                target,
                ..
            } => format!(
                "secret:resolve:{}:{}:{}",
                provider,
                reference,
                path_str(target)
            ),
            SecretAction::ResolveEnv {
                provider,
                reference,
                envs,
                ..
            } => format!(
                "secret:resolve-env:{}:{}:[{}]",
                provider,
                reference,
                envs.join(",")
            ),
            SecretAction::Skip { source, .. } => format!("secret:skip:{}", source),
        },
        Action::System(sa) => match sa {
            SystemAction::SetValue {
                configurator, key, ..
            } => format!("system:{}", system_resource_key(configurator, key)),
            SystemAction::Skip { configurator, .. } => {
                format!("system:{}:skip", configurator)
            }
        },
        Action::Script(sa) => match sa {
            // Resource-id / state-matching key, NOT a display string: this
            // return value is the SQLite `managed_resource` id and the
            // `ActionResult.description` JSON field. Condensing `run_str()`
            // here would reshape the id and break drift matching against
            // every already-recorded state row for a module with a
            // multi-line inline script — leave it byte-identical.
            ScriptAction::Run { entry, phase, .. } => {
                format!("script:{}:{}", phase.display_name(), entry.run_str())
            }
        },
        Action::Module(ma) => match &ma.kind {
            ModuleActionKind::InstallPackages { resolved } => {
                let names: Vec<&str> = resolved.iter().map(|p| p.resolved_name.as_str()).collect();
                format!("module:{}:packages:{}", ma.module_name, names.join(","))
            }
            ModuleActionKind::DeployFiles { declared_total, .. } => {
                module_files_description(&ma.module_name, *declared_total)
            }
            ModuleActionKind::RunScript { .. } => {
                format!("module:{}:script", ma.module_name)
            }
            ModuleActionKind::Skip { .. } => {
                format!("module:{}:skip", ma.module_name)
            }
        },
        Action::Env(ea) => match ea {
            EnvAction::WriteEnvFile { path, .. } => {
                format!("env:write:{}", path_str(path))
            }
            EnvAction::InjectSourceLine { rc_path, .. } => {
                format!("env:inject:{}", path_str(rc_path))
            }
            EnvAction::RefreshLiveSession { .. } => LIVE_SESSION_RESOURCE_ID.to_string(),
        },
        // The DAG's own node id: an edge names the string the journal row was
        // written under, so a dependent can be resolved against a completed
        // node without a second naming scheme.
        Action::Manager(ma) => ma.node_id(),
    }
}

/// Condense `desc` for a status-subject/error-message/bullet display only
/// when `action` carries something the wire keeps whole and a row cannot:
/// a raw, potentially multi-line script body (`format_action_description`'s
/// `Action::Script` arm and `format_module_action_body`'s
/// `ModuleActionKind::RunScript` arm both embed `run_str()` verbatim), or an
/// operand LIST longer than a row states (every package and file subject,
/// cut by [`elided_list`] into [`action_display_subject`]). `-o json`
/// payloads and `ActionResult.description` stay byte-identical to the
/// source body and name every operand. Callers must keep the raw `desc` for
/// `ActionResult.description` / journal persistence / the `-o json` plan
/// payload — this helper is display-only.
pub fn condense_action_desc_for_display(action: &Action, desc: &str) -> String {
    let embeds_raw_script = matches!(action, Action::Script(_))
        || matches!(
            action,
            Action::Module(ModuleAction {
                kind: ModuleActionKind::RunScript { .. },
                ..
            })
        );
    if embeds_raw_script {
        crate::output::condense_script_label(desc)
    } else if carries_operand_list(action) {
        action_display_subject(action).body
    } else {
        desc.to_string()
    }
}

/// Whether `action`'s subject is built over an operand LIST — the shapes
/// whose display subject is cut by [`elided_list`] while their wire string
/// names every operand.
fn carries_operand_list(action: &Action) -> bool {
    matches!(
        action,
        Action::Package(PackageAction::Install { .. } | PackageAction::Uninstall { .. })
            | Action::Module(ModuleAction {
                kind: ModuleActionKind::InstallPackages { .. }
                    | ModuleActionKind::DeployFiles { .. },
                ..
            })
    )
}

/// How a subject builder renders an operand list: in full for a WIRE string
/// (`PlanActionOutput.description`, `ActionResult.description`), or cut with
/// a `+N more` marker for a ROW ([`action_display_subject`]), the cut placed
/// where the row's `budget` runs out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListRender {
    Full,
    Elided { budget: Option<usize> },
}

impl ListRender {
    /// `prefix` followed by the list, the list cut to what fits the subject
    /// budget BESIDE the prefix — `brew install ` is part of the row, so the
    /// names are given the room the verb leaves, not the whole line.
    fn after(self, prefix: &str, items: &[String]) -> String {
        match self {
            ListRender::Full => format!("{prefix}{}", items.join(", ")),
            ListRender::Elided { budget } => {
                let room = budget.map(|b| b.saturating_sub(crate::output::measure_width(prefix)));
                format!("{prefix}{}", elided_list(items, SUBJECT_LIST_KEEP, room))
            }
        }
    }
}

/// An action's display subject, split at the marker the tree paints in its own
/// style.
///
/// Rendered, it is ONE string: `<marker>: <body>`, or just `<body>` when no
/// marker applies (every action kind but a script). The split exists because
/// the status renderer styles the marker and leaves the body in the terminal
/// foreground — not because the two halves may be composed differently at
/// different sites.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplaySubject {
    /// Rendered first, followed by `: `. `None` for everything but a script.
    pub marker: Option<String>,
    pub body: String,
}

impl std::fmt::Display for DisplaySubject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.marker {
            Some(marker) => write!(f, "{marker}: {}", self.body),
            None => f.write_str(&self.body),
        }
    }
}

/// The ONE display subject of an action.
///
/// The preview bullet, the phase's alignment column and the execution tree's
/// status line all derive from this, so the three cannot disagree about what an
/// action is called — a shorter executed subject silently mis-pads every
/// trailing field in the phase, and a preview that names a different string
/// than the execution is a lie about what ran.
///
/// Display-only. The persisted strings — the `managed_resources` id, the
/// journal `resource_id`, `ActionResult.description` and the `-o json` plan
/// payload — stay byte-identical to the source body and come from
/// [`format_action_description`] / [`format_plan_item`] instead.
pub fn action_display_subject(action: &Action) -> DisplaySubject {
    action_display_subject_within(action, None)
}

/// [`action_display_subject`] for a row that knows how wide it may be.
///
/// `budget` is the columns the subject may occupy — [`Printer::subject_budget`]
/// on the sink the row is drawn to — and an operand list fills it before it
/// cuts, so a wide terminal names as many packages as fit and a narrow one
/// still names [`SUBJECT_LIST_KEEP`]. `None` is the floor alone, the answer a
/// capture or a redirected stream gets, and every surface that renders ONE
/// report reads the same budget: the preview bullet, the alignment column,
/// the apply ledger, the live tree and the lane dispatcher's wait lines, so
/// one action is still one string wherever it is painted.
///
/// [`Printer::subject_budget`]: crate::output::Printer::subject_budget
pub fn action_display_subject_within(action: &Action, budget: Option<usize>) -> DisplaySubject {
    match action {
        Action::Script(ScriptAction::Run {
            entry,
            phase,
            origin,
        }) => script_run_subject_within(entry.run_str(), phase, origin, budget),
        Action::Module(
            ma @ ModuleAction {
                kind: ModuleActionKind::RunScript { script, phase },
                ..
            },
        ) => module_script_subject_within(script.run_str(), phase, ma.origin.as_deref(), budget),
        // The fold is the DISPLAY seam's alone: `ListRender::Full` feeds the
        // `-o json` plan payload and keeps the absolute path.
        _ => DisplaySubject {
            marker: None,
            body: crate::fold_home_in_text(&plan_item(action, ListRender::Elided { budget })),
        },
    }
}

/// [`action_display_subject`] for a profile script, reachable from the apply
/// path that holds the `ScriptAction`'s parts rather than the `Action`.
pub fn script_run_subject(run: &str, phase: &ScriptPhase, origin: &str) -> DisplaySubject {
    script_run_subject_within(run, phase, origin, None)
}

/// [`script_run_subject`] cut to `budget`, the same budget the operand lists
/// of the report are cut to; the apply path reads it off the printer, which
/// answers the claimed report budget while the run holds one.
pub fn script_run_subject_within(
    run: &str,
    phase: &ScriptPhase,
    origin: &str,
    budget: Option<usize>,
) -> DisplaySubject {
    let marker = format!("run {} script", phase.display_name());
    let body = script_body_display(run, origin, budget, &marker);
    DisplaySubject {
        marker: Some(marker),
        body,
    }
}

/// [`action_display_subject`] for a module script — the module's own hook name
/// is the marker, matching `format_module_action_body`'s `RunScript` arm.
pub fn module_script_subject(
    run: &str,
    phase: &ScriptPhase,
    origin: Option<&str>,
) -> DisplaySubject {
    module_script_subject_within(run, phase, origin, None)
}

/// [`module_script_subject`] cut to `budget`; see [`script_run_subject_within`].
pub fn module_script_subject_within(
    run: &str,
    phase: &ScriptPhase,
    origin: Option<&str>,
    budget: Option<usize>,
) -> DisplaySubject {
    let marker = phase.display_name().to_string();
    let body = script_body_display(run, origin.unwrap_or(""), budget, &marker);
    DisplaySubject {
        marker: Some(marker),
        body,
    }
}

/// The display subject of a hook script that has no planned `Action` behind it
/// — the daemon's `onDrift` hooks and the backup engine's `preBackup` /
/// `postBackup` hooks, which run outside a plan.
///
/// One derivation for two readers that must agree byte-for-byte: the caller
/// that opens the pseudo-phase derives its alignment column from this string
/// BEFORE any script runs, and `ScriptStatus` composes the very same string
/// onto the status line as each script finishes. Two copies of the format mis-
/// pad every line in the group the moment either one moves.
pub fn hook_script_subject(marker: &str, run: &str) -> DisplaySubject {
    DisplaySubject {
        marker: Some(marker.to_string()),
        body: crate::output::condense_script_label(run),
    }
}

/// The subject of a script with neither a planned action nor a hook marker:
/// the condensed body alone.
pub fn bare_script_subject(run: &str) -> DisplaySubject {
    DisplaySubject {
        marker: None,
        body: crate::output::condense_script_label(run),
    }
}

/// Condense the body, then append provenance: a long or multi-line script body
/// must not be able to truncate away the source that delivered it.
///
/// With a `budget` the body is cut so the WHOLE subject — `marker: body
/// <- origin` — fits it, floored at `SCRIPT_LABEL_MIN_CHARS`; the fixed
/// `SCRIPT_LABEL_MAX_CHARS` cap still binds on a wide screen.
fn script_body_display(run: &str, origin: &str, budget: Option<usize>, marker: &str) -> String {
    let suffix = provenance_suffix(origin);
    // The marker and its `: `, the provenance, and the `…` a cut appends.
    let framing =
        crate::output::measure_width(marker) + 2 + crate::output::measure_width(&suffix) + 1;
    let cap = budget.map_or(crate::output::SCRIPT_LABEL_MAX_CHARS, |b| {
        crate::output::SCRIPT_LABEL_MAX_CHARS.min(b.saturating_sub(framing))
    });
    format!(
        "{}{}",
        crate::output::condense_script_label_within(run, cap),
        suffix
    )
}

/// Format one plan item for display.
pub fn format_plan_item(action: &Action) -> String {
    plan_item(action, ListRender::Full)
}

/// [`format_plan_item`] with the operand lists rendered as `render` says.
fn plan_item(action: &Action, render: ListRender) -> String {
    match action {
        Action::File(fa) => match fa {
            FileAction::Create { target, origin, .. } => {
                format!("create {}{}", target.posix(), provenance_suffix(origin))
            }
            FileAction::Update { target, origin, .. } => {
                format!("update {}{}", target.posix(), provenance_suffix(origin))
            }
            FileAction::Delete { target, origin, .. } => {
                format!("delete {}{}", target.posix(), provenance_suffix(origin))
            }
            FileAction::SetPermissions {
                target,
                mode,
                origin,
                ..
            } => format!(
                "chmod {:#o} {}{}",
                mode,
                target.posix(),
                provenance_suffix(origin)
            ),
            FileAction::Skip {
                target,
                reason,
                origin,
                ..
            } => format!(
                "skip {}: {}{}",
                target.posix(),
                reason,
                provenance_suffix(origin)
            ),
        },
        Action::Package(pa) => match pa {
            PackageAction::Install {
                manager,
                packages,
                origin,
                ..
            } => format!(
                "{}{}",
                render.after(&format!("{manager} install "), packages),
                provenance_suffix(origin)
            ),
            PackageAction::Uninstall {
                manager,
                packages,
                origin,
                ..
            } => format!(
                "{}{}",
                render.after(&format!("{manager} uninstall "), packages),
                provenance_suffix(origin)
            ),
            PackageAction::Skip {
                manager,
                reason,
                origin,
                ..
            } => format!("skip {}: {}{}", manager, reason, provenance_suffix(origin)),
        },
        Action::Secret(sa) => match sa {
            SecretAction::Decrypt {
                source,
                target,
                backend,
                origin,
                ..
            } => format!(
                "decrypt {} → {} (via {}){}",
                source.posix(),
                target.posix(),
                backend,
                provenance_suffix(origin)
            ),
            SecretAction::Resolve {
                provider,
                reference,
                target,
                origin,
                ..
            } => format!(
                "resolve {}://{} → {}{}",
                provider,
                reference,
                target.posix(),
                provenance_suffix(origin)
            ),
            SecretAction::ResolveEnv {
                provider,
                reference,
                envs,
                origin,
                ..
            } => format!(
                "resolve {}://{} → env [{}]{}",
                provider,
                reference,
                envs.join(", "),
                provenance_suffix(origin)
            ),
            SecretAction::Skip {
                source,
                reason,
                origin,
                ..
            } => format!("skip {}: {}{}", source, reason, provenance_suffix(origin)),
        },
        Action::System(sa) => match sa {
            SystemAction::SetValue {
                configurator,
                key,
                desired,
                current,
                origin,
                ..
            } => format!(
                "set {}.{}: {} → {}{}",
                configurator,
                key,
                current,
                desired,
                provenance_suffix(origin)
            ),
            SystemAction::Skip {
                configurator,
                reason,
                unknown,
                ..
            } => {
                if *unknown {
                    format!(
                        "unknown system key '{}' — no such configurator (ignored)",
                        configurator
                    )
                } else {
                    format!("skip {}: {}", configurator, reason)
                }
            }
        },
        Action::Script(sa) => match sa {
            ScriptAction::Run {
                entry,
                phase,
                origin,
                ..
            } => {
                // Raw body: this same `Vec<String>` feeds both
                // `ApplyRun::preview` (human bullets) AND `build_plan_output`'s
                // `PlanActionOutput.description` (the `-o json` plan
                // payload). Condensing here would truncate the JSON
                // payload too — display sites condense for themselves via
                // `condense_action_desc_for_display`.
                format!(
                    "run {} script: {}{}",
                    phase.display_name(),
                    entry.run_str(),
                    provenance_suffix(origin)
                )
            }
        },
        Action::Module(ma) => module_action_item(ma, render),
        Action::Env(ea) => match ea {
            EnvAction::WriteEnvFile { path, .. } => {
                format!("write {}", path.posix())
            }
            EnvAction::InjectSourceLine { rc_path, .. } => {
                format!("inject source line into {}", rc_path.posix())
            }
            EnvAction::RefreshLiveSession { vars } => {
                format!(
                    "publish {} to the live session",
                    crate::pluralize(vars.len(), "var")
                )
            }
        },
        Action::Manager(ma) => format_manager_action_item(ma),
    }
}

/// Format a manager action for plan display.
///
/// Imperative like every other plan item, so the preview and the executed line
/// read as the same statement about the same work. A prerequisite names who
/// needed it: the tool is not in the user's package set and the line is the
/// only place that explains why cfgd is installing it.
fn format_manager_action_item(action: &ManagerAction) -> String {
    match action {
        ManagerAction::RefreshIndex { manager } => format!("refresh {manager} index"),
        // A batch names every manager the one command delivers, in the order
        // `provisioned_managers` holds them — the line has to account for what
        // it installs, and `provision npm via apt` would silently also install
        // pipx. A declared route names its package for the same reason: the
        // command a module's `aliases: {apt: rustc}` produces is `apt-get
        // install rustc`, and a line saying only `cargo` sends a reader whose
        // alias cannot provide the tool looking for a cfgd bug instead of at
        // the entry they wrote. Suppressed when the route's package IS the
        // manager's name — there the operand is already in the sentence, and
        // `provision cargo via brew (cargo)` says one word twice.
        ManagerAction::Provision { via, declared, .. } => {
            let managers = action.provisioned_managers().join(", ");
            match declared
                .as_ref()
                .map(|route| route.package.as_str())
                .filter(|package| *package != managers)
            {
                Some(package) => format!("provision {managers} via {via} ({package})"),
                None => format!("provision {managers} via {via}"),
            }
        }
        ManagerAction::Prerequisite {
            tool,
            installer,
            required_by,
            ..
        } => format!(
            "{installer} install {tool} — required by {}",
            required_by.join(", ")
        ),
        ManagerAction::Refuse { manager, reason } => {
            format!("cannot provision {manager} — {reason}")
        }
    }
}

/// Format one owner group's plan items for display, one per action in order.
pub fn format_plan_items(group: &OwnerGroup) -> Vec<String> {
    group.actions.iter().map(format_plan_item).collect()
}

/// Format a module action for plan display.
///
/// Source-delivered modules (`origin = Some`) get the same ` <- <source>`
/// provenance suffix as source-delivered files/packages; consumer-local modules
/// (`origin = None`) render with no suffix.
#[cfg(test)]
pub(super) fn format_module_action_item(action: &ModuleAction) -> String {
    module_action_item(action, ListRender::Full)
}

fn module_action_item(action: &ModuleAction, render: ListRender) -> String {
    let suffix = provenance_suffix(action.origin.as_deref().unwrap_or(""));
    let body = format_module_action_body(action, render);
    format!("{body}{suffix}")
}

fn format_module_action_body(action: &ModuleAction, render: ListRender) -> String {
    match &action.kind {
        ModuleActionKind::InstallPackages { resolved } => {
            // Group by manager in first-appearance order: this string is also
            // the persisted plan/description payload, so its manager segments
            // must be deterministic across runs (a HashMap here reshuffled
            // multi-manager modules on every plan).
            let mut by_manager: Vec<(&str, Vec<String>)> = Vec::new();
            for pkg in resolved {
                let display = if let Some(ref ver) = pkg.version {
                    if pkg.canonical_name != pkg.resolved_name {
                        format!(
                            "{} ({}, alias: {})",
                            pkg.resolved_name, ver, pkg.canonical_name
                        )
                    } else {
                        format!("{} ({})", pkg.resolved_name, ver)
                    }
                } else if pkg.canonical_name != pkg.resolved_name {
                    format!("{} (alias: {})", pkg.resolved_name, pkg.canonical_name)
                } else {
                    pkg.resolved_name.clone()
                };
                match by_manager.iter_mut().find(|(mgr, _)| *mgr == pkg.manager) {
                    Some((_, pkgs)) => pkgs.push(display),
                    None => by_manager.push((&pkg.manager, vec![display])),
                }
            }
            let parts: Vec<String> = by_manager
                .iter()
                .map(|(mgr, pkgs)| render.after(&format!("{mgr} install "), pkgs))
                .collect();
            parts.join("; ")
        }
        ModuleActionKind::DeployFiles { files, .. } => {
            // The subject names the targets only. How many the deploy writes,
            // and against how many the module declares, is a fact the step
            // PRODUCES and so is the row's detail (`deploy_files_summary`),
            // the slot the sibling env-write row already puts its counts in.
            let targets: Vec<String> = files.iter().map(|f| f.target.display_posix()).collect();
            render.after("deploy ", &targets)
        }
        ModuleActionKind::RunScript { script, phase } => {
            // Raw body: this same string feeds both `ApplyRun::preview`
            // (human bullets) AND `build_plan_output`'s `PlanActionOutput.description`
            // (the `-o json` plan payload), each through `format_plan_item` ->
            // `format_module_action_item`. Condensing here would truncate the
            // JSON payload too — display sites condense for themselves via
            // `condense_action_desc_for_display`.
            format!("{}: {}", phase.display_name(), script.run_str())
        }
        ModuleActionKind::Skip { reason } => {
            format!("skip: {reason}")
        }
    }
}

/// Join `items`, naming at most `keep` of them and saying how many were left
/// out (`a, b, +4 more`).
///
/// The marker is not decoration: this string is the action's subject in every
/// tree, so a silent cut left `deploy …/init.lua, …/lazy-lock.json — 6 files`
/// reading as two deploys that produced six files, beside sibling rows naming
/// all twelve of their own operands. A list short enough to state in full
/// carries no marker, having elided nothing.
///
/// DISPLAY only, through [`ListRender::Elided`]: the wire strings —
/// `PlanActionOutput.description` in `-o json`, `ActionResult.description`,
/// the recorded resource id — name every operand ([`ListRender::Full`]). The
/// cut once lived in the builders that feed both, so `cfgd plan -o json`
/// emitted `apt install unzip, ripgrep, +9 more` and nine names reached no
/// wire at all: `action_targets` is empty for both package shapes, so nothing
/// compensated the way `targets` does for a deploy.
/// `every_operand_a_plan_action_holds_reaches_the_json_payload` (the cfgd
/// crate) walks every list-bearing shape's serialized payload.
///
/// The ONE elision in this file, read by every subject over an operand LIST
/// — a module's package segments (one cut per manager, so neither list
/// vanishes behind the other's marker), its file targets, and a profile's
/// own install and uninstall. A list named in full instead was cut by the
/// terminal mid-token (`…, eza, carg…`) with no count anywhere. Every other
/// subject builder names every operand it holds:
/// `every_elided_operand_list_says_so` walks the module-action kinds and
/// `every_operand_list_a_subject_renders_is_cut_with_a_marker` the rendered
/// string of every list-bearing shape.
/// How many operands a subject names AT LEAST before `elided_list` cuts, and
/// so the ONE threshold every detail arm reasons about: a list at most one
/// longer is stated in full, and a longer one carries `+N more`, which with
/// the named operands already gives the total. A FLOOR, not the count: a row
/// that knows its width ([`action_display_subject_within`]) names every
/// operand that fits before the marker, so a 120-column terminal is not cut
/// to two names by a constant chosen for the narrowest one. A produced-detail arm therefore never
/// restates a full count over an elided subject — `deploy a, b, +4 more —
/// 6 files` said six twice — and the walk
/// `no_produced_detail_restates_a_total_the_subject_already_gives` renders
/// every arm over `SUBJECT_LIST_KEEP + 3` operands to keep it so.
pub(super) const SUBJECT_LIST_KEEP: usize = 2;

fn elided_list(items: &[String], keep: usize, room: Option<usize>) -> String {
    if items.len() <= keep + 1 {
        return items.join(", ");
    }
    let cut = |named: usize| {
        format!(
            "{}, +{} more",
            items[..named].join(", "),
            items.len() - named
        )
    };
    let Some(room) = room else {
        return cut(keep);
    };
    let full = items.join(", ");
    if crate::output::measure_width(&full) <= room {
        return full;
    }
    // Fill towards the room, never below the floor: the marker stays honest
    // because it is written only over the names it does not hold.
    let mut named = keep;
    while named + 1 < items.len() && crate::output::measure_width(&cut(named + 1)) <= room {
        named += 1;
    }
    cut(named)
}

/// Prefixes whose `format_action_description`/`execute_script` output has a
/// structural-colon count fixed at TWO (`type:subtype:body`) for every variant
/// of that action kind. Only for those is the second segment safely droppable:
/// it names the verb/phase that produced the row (`create`/`update`/`delete`
/// for `file`, `pre`/`post` for `script`), never an identity, and drift/state
/// matching keys on `(type, body)` so two verbs touching the same target share
/// one id.
///
/// Excluded prefixes either vary their structural-colon count or carry an
/// identity in the second segment, so no fixed split is correct for them:
/// - `module` names the MODULE in its second segment (`module:{name}:{verb}`),
///   not a verb — dropping it would collapse every module onto one
///   `UNIQUE(resource_type, resource_id)` row.
/// - `package` names the MANAGER (`package:{manager}:{verb}`) — same collapse:
///   `package:brew:skip` and `package:apt:skip` would share one row. Only
///   `bootstrap` and `skip` reach here; `install`/`uninstall` are handled
///   ahead of this parser, per-package, by `parse_package_description`.
/// - `system` stamps `system:{configurator}.{key}` (one colon) but
///   `system:{configurator}:skip` (two).
/// - `execute_script`'s `"Running script: {body}"` has one.
///
/// A blind `splitn(3, ':')` cannot tell "2 structural colons" apart from
/// "1 structural colon + a colon embedded in the body" — both consume 2 colons
/// and yield 3 pieces. Dispatching on the known prefix (rather than counting
/// colons) keeps the body intact either way: a `run:` script or `-o json` value
/// containing its own `:` no longer gets silently truncated mid-body.
const TWO_COLON_PREFIXES: &[&str] = &["file", "secret", "script", "env"];

/// The action description a module's file deployment is recorded under — one
/// aggregate for the whole module, keyed on the DECLARED file count so a
/// partial deploy lands on the same `managed_resources` row a full deploy
/// wrote. Three sites mint this string and match it against each other (the
/// resource id here, the apply arm's returned description, and the recorded-hash
/// refresh), so a byte of divergence between any two of them records state
/// nothing ever reads back.
pub(super) fn module_files_description(module_name: &str, declared_total: usize) -> String {
    format!("module:{module_name}:files:{declared_total}")
}

pub(super) fn parse_resource_from_description(desc: &str) -> (String, String) {
    let Some((prefix, rest)) = desc.split_once(':') else {
        return ("unknown".to_string(), desc.to_string());
    };
    if TWO_COLON_PREFIXES.contains(&prefix) {
        let id = rest.split_once(':').map_or(rest, |(_, id)| id);
        (prefix.to_string(), id.to_string())
    } else {
        (prefix.to_string(), rest.to_string())
    }
}

/// Parse a package action description (`"package:<mgr>:<verb>:<csv-packages>"`)
/// into `(manager, verb, packages)`. Returns `None` for any description that is
/// not a package action or lacks a package list (e.g. `package:<mgr>:bootstrap`,
/// `package:<mgr>:skip`), so per-package tracking only fires for install/uninstall.
pub(super) fn parse_package_description(desc: &str) -> Option<(String, String, Vec<String>)> {
    let parts: Vec<&str> = desc.splitn(4, ':').collect();
    if parts.len() != 4 || parts[0] != "package" {
        return None;
    }
    let manager = parts[1].to_string();
    let verb = parts[2].to_string();
    let packages: Vec<String> = parts[3].split(',').map(str::to_string).collect();
    Some((manager, verb, packages))
}

#[cfg(test)]
mod tests {
    use super::super::types::{
        Action, DeclaredProvision, EnvAction, ManagerAction, ModuleAction, ModuleActionKind,
    };
    use crate::providers::PackageAction;

    use super::{
        SUBJECT_LIST_KEEP, action_display_subject, action_display_subject_within, elided_list,
        format_manager_action_item, format_module_action_item, parse_package_description,
        pre_skip_doubling_error,
    };

    /// No withheld row states one noun twice.
    ///
    /// A pre-skipped action renders as `<subject> — <reason>`, two slots the
    /// withholding role deliberately paints in opposite emphasis, and
    /// `publish 3 vars to the session manager — no session manager` spent both
    /// of them on the same noun. The same defect
    /// `system_key_doubling_error` and `note_tag_doubling_error` already
    /// forbid on the other two name-prefixed compositions.
    ///
    /// The table is every action `Action::pre_skip_reason` answers `Some` for.
    /// Its completeness is asserted against that function's own source, so a
    /// second gate of that shape is classified here before it can ship a
    /// doubled row.
    #[test]
    fn no_pre_skip_reason_repeats_a_noun_its_subject_already_names() {
        let withheld: Vec<(Action, &str)> = vec![(
            Action::Env(EnvAction::RefreshLiveSession {
                vars: vec![("EDITOR".into(), "nvim".into())],
            }),
            crate::NO_SESSION_MANAGER,
        )];

        for (action, reason) in &withheld {
            let subject = action_display_subject(action).to_string();
            assert!(
                pre_skip_doubling_error(&subject, reason).is_none(),
                "{}",
                pre_skip_doubling_error(&subject, reason).unwrap_or_default()
            );
        }

        let types = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/reconciler/types.rs"),
        )
        .expect("types.rs is readable");
        let body = types
            .split_once("pub fn pre_skip_reason(")
            .expect("pre_skip_reason is declared")
            .1
            .split_once("\n    }\n")
            .expect("pre_skip_reason has a body")
            .0;
        assert_eq!(
            body.matches("Some(").count(),
            withheld.len(),
            "a pre-skip arm was added without a row in this walk: {body}"
        );
    }

    /// The elision marker `elided_list` mints, as a reader sees it.
    const ELIDED: &str = " more";

    /// A subject that cuts its operand list SAYS it cut it.
    ///
    /// The one string is the plan preview bullet, the alignment column, the
    /// executed row AND `PlanActionOutput.description`, so a silent cut is a
    /// silent cut in all four: `deploy …/init.lua, …/lazy-lock.json — 6 files`
    /// read as two deploys producing six files, beside sibling rows naming all
    /// twelve of their own operands, and a structured consumer had no way to
    /// tell the list was short.
    ///
    /// Every `ModuleActionKind` is walked, each with more operands than any
    /// builder here keeps, and each must either name all of them or say how
    /// many it left out. Bound with no `..`, so a new kind is classified before
    /// this file compiles.
    #[test]
    fn every_elided_operand_list_says_so() {
        let file = |target: &str| crate::modules::ResolvedFile {
            source: std::path::PathBuf::from("src"),
            target: std::path::PathBuf::from(target),
            is_git_source: false,
            strategy: None,
            encryption: None,
            permissions: None,
            patch: None,
        };
        let pkg = |name: &str| crate::modules::ResolvedPackage {
            canonical_name: name.to_string(),
            resolved_name: name.to_string(),
            manager: "brew".to_string(),
            manager_declared: false,
            version: None,
            script: None,
            creates: None,
            only_if: None,
            unless: None,
            min_version: None,
        };
        let names = ["a", "b", "c", "d", "e", "f"];
        let kinds = [
            ModuleActionKind::InstallPackages {
                resolved: names.iter().map(|n| pkg(n)).collect(),
            },
            ModuleActionKind::DeployFiles {
                files: names.iter().map(|n| file(n)).collect(),
                declared_total: names.len(),
            },
            ModuleActionKind::RunScript {
                script: crate::config::ScriptEntry::Simple(names.join(" && ")),
                phase: crate::reconciler::ScriptPhase::PostApply,
            },
            ModuleActionKind::Skip {
                reason: names.join(", "),
            },
        ];
        for kind in kinds {
            let subject = format_module_action_item(&ModuleAction::local("m", kind));
            let named = names.iter().filter(|n| subject.contains(**n)).count();
            assert!(
                named == names.len() || subject.contains(ELIDED),
                "a subject that names only {named} of {} operands must say so: {subject}",
                names.len()
            );
        }
    }

    /// Every subject over an operand LIST cuts it at `SUBJECT_LIST_KEEP` and
    /// says so on the RENDERED string — the one every tree paints, and the
    /// one a live row clamps to its width. Naming all of them had a module's
    /// package row read `brew install neovim, ripgrep, fd, bat, eza, carg…`,
    /// cut mid-token by the terminal with no count anywhere; the marker is
    /// what a reader gets instead of the clamp. Walked over every list-
    /// bearing shape: the module package and file kinds, and a profile's own
    /// install and uninstall; a module naming two managers is cut per manager
    /// segment, so neither manager's list vanishes behind the other's marker.
    #[test]
    fn every_operand_list_a_subject_renders_is_cut_with_a_marker() {
        let pkg = |name: &str, manager: &str| crate::modules::ResolvedPackage {
            canonical_name: name.to_string(),
            resolved_name: name.to_string(),
            manager: manager.to_string(),
            manager_declared: false,
            version: None,
            script: None,
            creates: None,
            only_if: None,
            unless: None,
            min_version: None,
        };
        let file = |target: &str| crate::modules::ResolvedFile {
            source: std::path::PathBuf::from("src"),
            target: std::path::PathBuf::from(target),
            is_git_source: false,
            strategy: None,
            encryption: None,
            permissions: None,
            patch: None,
        };
        let total = SUBJECT_LIST_KEEP + 3;
        let names: Vec<String> = (0..total).map(|i| format!("op{i}")).collect();
        let package = |manager: &str| PackageAction::Install {
            manager: manager.to_string(),
            packages: names.clone(),
            origin: "local".to_string(),
        };
        let actions = [
            Action::Module(ModuleAction::local(
                "m",
                ModuleActionKind::InstallPackages {
                    resolved: names
                        .iter()
                        .map(|n| pkg(n, "brew"))
                        .chain(names.iter().map(|n| pkg(n, "apt")))
                        .collect(),
                },
            )),
            Action::Module(ModuleAction::local(
                "m",
                ModuleActionKind::DeployFiles {
                    files: names.iter().map(|n| file(n)).collect(),
                    declared_total: total,
                },
            )),
            Action::Package(package("brew")),
            Action::Package(PackageAction::Uninstall {
                manager: "brew".to_string(),
                packages: names.clone(),
                origin: "local".to_string(),
            }),
        ];
        for action in &actions {
            let subject = action_display_subject(action).to_string();
            let segments = subject.matches(" install ").count()
                + subject.matches(" uninstall ").count()
                + usize::from(subject.starts_with("deploy "));
            let marker = format!("+{} more", total - SUBJECT_LIST_KEEP);
            assert_eq!(
                subject.matches(&marker).count(),
                segments.max(1),
                "every operand list on the row is cut with its own marker: {subject}"
            );
            assert!(
                !subject.contains(&names[SUBJECT_LIST_KEEP + 1]),
                "an operand past the keep is not named: {subject}"
            );
        }
    }

    /// A list short enough to state in full elided nothing, so it says nothing.
    #[test]
    fn a_list_that_fits_carries_no_elision_marker() {
        let items: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        assert_eq!(elided_list(&items, 2, None), "a, b, c");
        assert_eq!(elided_list(&items[..1], 2, None), "a");
        assert_eq!(
            elided_list(
                &["a", "b", "c", "d"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>(),
                2,
                None
            ),
            "a, b, +2 more"
        );
    }

    /// A row that knows its width names every operand that fits before the
    /// marker, never fewer than the floor, and drops the marker only when the
    /// whole list fits — so `+N more` is written over names the row does
    /// not hold, and over nothing else.
    #[test]
    fn a_row_with_room_names_as_many_operands_as_fit_and_never_fewer_than_the_floor() {
        let items: Vec<String> = ["neovim", "fd", "ripgrep", "bat", "fzf", "jq"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            elided_list(&items, 2, Some(80)),
            "neovim, fd, ripgrep, bat, fzf, jq",
            "everything fits: no marker"
        );
        assert_eq!(
            elided_list(&items, 2, Some(30)),
            "neovim, fd, ripgrep, +3 more",
            "filled to the room, the marker counting exactly what is unnamed"
        );
        assert_eq!(
            elided_list(&items, 2, Some(5)),
            "neovim, fd, +4 more",
            "a room narrower than the floor still names the floor"
        );
        let widest = elided_list(&items, 2, Some(30));
        assert!(crate::output::measure_width(&widest) <= 30);

        let action = Action::Package(PackageAction::Install {
            manager: "brew".to_string(),
            packages: items.clone(),
            origin: "local".to_string(),
        });
        let budgeted = action_display_subject_within(&action, Some(44)).to_string();
        assert!(
            crate::output::measure_width(&budgeted) <= 44
                && budgeted.contains("ripgrep")
                && !budgeted.contains("bat"),
            "the prefix `brew install ` is charged against the same budget: {budgeted}"
        );
        assert_eq!(
            action_display_subject_within(&action, None).to_string(),
            "brew install neovim, fd, +4 more",
            "no budget is the floor"
        );
    }

    /// The operands a manager action holds — every name the subject has to
    /// account for. Bound field by field with no `..`, so a new field on any
    /// variant has to be classified here before this file compiles.
    fn operands(action: &ManagerAction) -> Vec<&str> {
        match action {
            ManagerAction::RefreshIndex { manager } => vec![manager.as_str()],
            ManagerAction::Provision {
                manager,
                via,
                declared,
                batched,
                depends_on: _,
            } => std::iter::once(manager.as_str())
                .chain(std::iter::once(via.as_str()))
                .chain(batched.iter().map(String::as_str))
                .chain(declared.iter().map(|route| route.package.as_str()))
                .collect(),
            ManagerAction::Prerequisite {
                tool,
                installer,
                required_by,
                depends_on: _,
            } => std::iter::once(tool.as_str())
                .chain(std::iter::once(installer.as_str()))
                .chain(required_by.iter().map(String::as_str))
                .collect(),
            ManagerAction::Refuse { manager, reason } => vec![manager.as_str(), reason.as_str()],
        }
    }

    /// A subject names every operand its action carries.
    ///
    /// The one string reaches the plan preview bullet, the alignment column,
    /// the executed row and `PlanActionOutput.description`, so an operand it
    /// drops is dropped from all four at once: `provision cargo via apt` for a
    /// route whose command is `apt-get install rustc` promised work no part of
    /// the run performed, and told the reader cfgd's apt provisioning was
    /// broken rather than that their own `aliases:` entry was.
    #[test]
    fn every_manager_action_subject_names_every_operand_it_holds() {
        let cases = [
            ManagerAction::RefreshIndex {
                manager: "sentinel-manager".into(),
            },
            // A declared route is never batched: the batch is one mediator
            // command over the manager's own names, so the two shapes are two
            // cases rather than one impossible action.
            ManagerAction::Provision {
                manager: "sentinel-manager".into(),
                via: "sentinel-via".into(),
                declared: Some(DeclaredProvision {
                    installer: "sentinel-via".into(),
                    package: "sentinel-package".into(),
                }),
                batched: Vec::new(),
                depends_on: Vec::new(),
            },
            ManagerAction::Provision {
                manager: "sentinel-manager".into(),
                via: "sentinel-via".into(),
                declared: None,
                batched: vec!["sentinel-batched".into()],
                depends_on: Vec::new(),
            },
            ManagerAction::Prerequisite {
                tool: "sentinel-tool".into(),
                installer: "sentinel-installer".into(),
                required_by: vec!["sentinel-requirer".into()],
                depends_on: Vec::new(),
            },
            ManagerAction::Refuse {
                manager: "sentinel-manager".into(),
                reason: "sentinel-reason".into(),
            },
        ];
        for action in &cases {
            let subject = format_manager_action_item(action);
            for operand in operands(action) {
                assert!(
                    subject.contains(operand),
                    "{action:?} holds {operand:?}, and its subject {subject:?} never names it"
                );
            }
        }
    }

    /// The one exception, and the reason it is not a dropped operand: a route
    /// whose package IS the manager's name has its operand in the sentence
    /// already.
    #[test]
    fn a_route_whose_package_is_the_manager_name_is_not_named_twice() {
        let subject = format_manager_action_item(&ManagerAction::Provision {
            manager: "cargo".into(),
            via: "brew".into(),
            declared: Some(DeclaredProvision {
                installer: "brew".into(),
                package: "cargo".into(),
            }),
            batched: Vec::new(),
            depends_on: Vec::new(),
        });
        assert_eq!(subject, "provision cargo via brew");
    }

    #[test]
    fn parse_package_install_single() {
        let parsed = parse_package_description("package:apt:install:hello").unwrap();
        assert_eq!(parsed.0, "apt");
        assert_eq!(parsed.1, "install");
        assert_eq!(parsed.2, vec!["hello".to_string()]);
    }

    #[test]
    fn parse_package_install_csv_multi() {
        let parsed =
            parse_package_description("package:cargo:install:bat,ripgrep,fd-find").unwrap();
        assert_eq!(parsed.0, "cargo");
        assert_eq!(parsed.1, "install");
        assert_eq!(
            parsed.2,
            vec![
                "bat".to_string(),
                "ripgrep".to_string(),
                "fd-find".to_string()
            ]
        );
    }

    #[test]
    fn parse_package_uninstall() {
        let parsed = parse_package_description("package:apt:uninstall:fd-find").unwrap();
        assert_eq!(parsed.1, "uninstall");
        assert_eq!(parsed.2, vec!["fd-find".to_string()]);
    }

    #[test]
    fn parse_package_bootstrap_and_skip_have_no_packages() {
        // bootstrap/skip descriptions carry no csv package list, so parsing
        // declines them — they never drive per-package tracking.
        assert!(parse_package_description("package:brew:bootstrap").is_none());
        assert!(parse_package_description("package:apt:skip").is_none());
    }

    #[test]
    fn parse_non_package_description_declines() {
        assert!(parse_package_description("file:create:/home/.zshrc").is_none());
    }
}
