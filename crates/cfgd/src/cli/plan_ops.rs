use super::*;
use cfgd_core::PathDisplayExt;
use cfgd_core::output::{Doc, Printer, Role};

// --- Plan output rendering ---

pub(in crate::cli) fn print_apply_result(
    result: &cfgd_core::reconciler::ApplyResult,
    printer: &Printer,
    elapsed: Option<std::time::Duration>,
) -> cfgd_core::state::ApplyStatus {
    // Partial splits into two role-tagged lines so the success count and the
    // failure count read as distinct outcomes — fusing them into one Warn line
    // makes a "9 succeeded, 1 failed" run look the same colour as a "1
    // succeeded, 9 failed" run.
    if matches!(result.status, cfgd_core::state::ApplyStatus::Partial) {
        printer.status_simple(
            Role::Ok,
            format!("{} action(s) succeeded", result.succeeded()),
        );
        let failed_subject = format!("{} action(s) failed", result.failed());
        match elapsed {
            Some(d) => {
                printer.status(Role::Accent, failed_subject).duration(d);
            }
            None => printer.status_simple(Role::Accent, failed_subject),
        }
        return result.status.clone();
    }

    let (role, subject) = match result.status {
        cfgd_core::state::ApplyStatus::Success => (
            Role::Ok,
            format!(
                "Apply complete — {} action(s) succeeded",
                result.succeeded()
            ),
        ),
        cfgd_core::state::ApplyStatus::Partial => unreachable!("handled above"),
        cfgd_core::state::ApplyStatus::Failed => (
            Role::Fail,
            format!("Apply failed — {} action(s) failed", result.failed()),
        ),
        cfgd_core::state::ApplyStatus::InProgress => (
            Role::Warn,
            "Apply still in progress (unexpected state)".to_string(),
        ),
        cfgd_core::state::ApplyStatus::Aborted => (
            Role::Warn,
            format!(
                "Apply aborted by signal — {} action(s) applied",
                result.succeeded()
            ),
        ),
    };
    match elapsed {
        Some(d) => {
            printer.status(role, subject).duration(d);
        }
        None => printer.status_simple(role, subject),
    }
    result.status.clone()
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
            PackageAction::Bootstrap { .. } => "bootstrap",
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
    }
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
        | reconciler::Action::Script(_) => vec![],
    }
}

/// Source provenance of a plan action for structured (`-o json`) consumers:
/// `Some(source_name)` when a ConfigSource delivered the resource body, `None`
/// for consumer-local resources (and for action kinds with no provenance, e.g.
/// system writes / env / locally-authored scripts). Files/packages/secrets/
/// scripts carry origin as the sentinel `String` `"local"`/`""`; modules carry
/// it as `Option<String>`. Both normalize to `None` for local here so the wire
/// field is omitted exactly when there is no remote provenance to report.
pub(in crate::cli) fn action_origin(action: &reconciler::Action) -> Option<String> {
    fn norm(origin: &str) -> Option<String> {
        if origin.is_empty() || origin == "local" {
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
            PackageAction::Bootstrap { origin, .. }
            | PackageAction::Install { origin, .. }
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
        reconciler::Action::Env(_) => None,
    }
}

/// Build a PlanOutput from a reconciler Plan, applying an optional phase filter.
pub(in crate::cli) fn build_plan_output(
    plan: &reconciler::Plan,
    context_name: &str,
    phase_filter: Option<&PhaseName>,
) -> PlanOutput {
    let mut phases = Vec::new();
    for phase_item in &plan.phases {
        if let Some(pf) = phase_filter
            && &phase_item.name != pf
        {
            continue;
        }
        let items = reconciler::format_plan_items(phase_item);
        let actions: Vec<PlanActionOutput> = phase_item
            .actions
            .iter()
            .zip(items.iter())
            .map(|(action, desc)| PlanActionOutput {
                description: desc.clone(),
                action_type: action_type_str(action).to_string(),
                targets: action_targets(action),
                origin: action_origin(action),
            })
            .collect();
        phases.push(PlanPhaseOutput {
            phase: phase_item.name.display_name().to_string(),
            actions,
        });
    }
    let total_actions = phases.iter().map(|p| p.actions.len()).sum();
    PlanOutput {
        context: context_name.to_string(),
        phases,
        total_actions,
        warnings: plan.warnings.clone(),
    }
}

/// Strip all script-related actions from a plan.
/// Removes PreScripts/PostScripts phases, module-level RunScript actions,
/// and script-based package installs (manager: "script").
pub(in crate::cli) fn strip_scripts_from_plan(plan: &mut reconciler::Plan) {
    plan.phases
        .retain(|p| !matches!(p.name, PhaseName::PreScripts | PhaseName::PostScripts));
    for phase in &mut plan.phases {
        if phase.name == PhaseName::Modules {
            phase.actions.retain(|a| match a {
                reconciler::Action::Module(reconciler::ModuleAction {
                    kind: reconciler::ModuleActionKind::RunScript { .. },
                    ..
                }) => false,
                reconciler::Action::Module(reconciler::ModuleAction {
                    kind: reconciler::ModuleActionKind::InstallPackages { resolved },
                    ..
                }) => resolved.first().is_none_or(|p| p.manager != "script"),
                _ => true,
            });
        }
    }
}

pub(in crate::cli) fn display_plan_table(
    plan: &reconciler::Plan,
    printer: &Printer,
    phase_filter: Option<&PhaseName>,
) {
    for phase_item in &plan.phases {
        if let Some(pf) = phase_filter
            && &phase_item.name != pf
        {
            continue;
        }
        let items = reconciler::format_plan_items(phase_item);
        let phase = printer.section(format!("Phase: {}", phase_item.name.display_name()));
        if items.is_empty() {
            phase.empty_state("(nothing to do)");
        } else {
            // `format_plan_items` yields one item per action in order, so the
            // zip stays aligned. An unknown system key (likely a typo) surfaces
            // as a real warning instead of a neutral bullet so it isn't missed.
            for (item, action) in items.iter().zip(&phase_item.actions) {
                if let reconciler::Action::System(reconciler::SystemAction::Skip {
                    unknown: true,
                    ..
                }) = action
                {
                    phase.status_simple(Role::Warn, item);
                } else {
                    phase.bullet(item);
                }
            }
        }
    }
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
                .filter(|p| !p.actions.is_empty())
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
pub(in crate::cli) fn report_no_in_scope_actions(
    printer: &Printer,
    scope: &ScopeReport,
    phase_filter: Option<&PhaseName>,
) {
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
    // The most common scoping mistake: `--phase files` against a config whose
    // files come from modules (those deploy in the Modules phase to keep each
    // module's files+packages+scripts atomic and dependency-ordered).
    if phase_filter == Some(&PhaseName::Files)
        && scope
            .phases_with_work
            .iter()
            .any(|p| p.as_str() == PhaseName::Modules.display_name())
    {
        printer.hint(
            "module-sourced files apply in the 'modules' phase — try `--phase modules` or `--module <name>`",
        );
    }
}

pub(in crate::cli) fn display_plan_preview(
    plan: &reconciler::Plan,
    printer: &Printer,
    state: &cfgd_core::state::StateStore,
    context: &str,
    phase_filter: Option<&PhaseName>,
    dry_run_fm: Option<&CfgdFileManager>,
    scope: &ScopeReport,
) {
    // Show pending decisions (not included in this plan)
    if let Ok(pending) = state.pending_decisions()
        && !pending.is_empty()
    {
        let section = printer.section("Pending Decisions (not included in this plan)");
        for d in &pending {
            section.status_simple(
                Role::Info,
                format!(
                    "{} {} — {} by {} (run `cfgd decide accept/reject`)",
                    d.tier, d.resource, d.action, d.source,
                ),
            );
        }
    }

    // Build structured output
    let plan_output = build_plan_output(plan, context, phase_filter);

    // Structured-output routing: when -o yaml/json/etc., emit the plan as the
    // doc's data payload and skip the human render.
    if printer.is_structured() {
        printer.emit(Doc::new().with_data(&plan_output));
        return;
    }

    // Table mode display
    display_plan_table(plan, printer, phase_filter);

    // Show diffs for file updates
    if let Some(fm) = dry_run_fm {
        for phase_item in &plan.phases {
            if phase_item.name != PhaseName::Files {
                continue;
            }
            for action in &phase_item.actions {
                if let reconciler::Action::File(FileAction::Update { source, target, .. }) = action
                    && let Ok(target_content) = std::fs::read_to_string(target)
                {
                    let source_content = if crate::files::is_tera_template(source) {
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

    for w in &plan.warnings {
        printer.status_simple(Role::Warn, w);
    }

    if plan_output.total_actions == 0 {
        report_no_in_scope_actions(printer, scope, phase_filter);
    } else {
        printer.status_simple(
            Role::Info,
            format!("{} action(s) planned", plan_output.total_actions),
        );
    }
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
                PackageAction::Bootstrap { manager, .. } => manager,
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
            format!("{}:{}", prefix, target.display())
        }
        reconciler::Action::Secret(sa) => match sa {
            SecretAction::Decrypt { target, .. } => {
                format!("{}:{}", prefix, target.display())
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
            format!("{}.{}", prefix, ma.module_name)
        }
        reconciler::Action::Env(ea) => match ea {
            reconciler::EnvAction::WriteEnvFile { path, .. } => {
                format!("{}:{}", prefix, path.display())
            }
            reconciler::EnvAction::InjectSourceLine { rc_path, .. } => {
                format!("{}:{}", prefix, rc_path.display())
            }
            reconciler::EnvAction::RefreshLiveSession { .. } => {
                format!("{}:live-session", prefix)
            }
        },
    }
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
        let mut i = 0;
        while i < phase.actions.len() {
            // Profile file actions
            if let reconciler::Action::File(
                FileAction::Create { target, .. } | FileAction::Update { target, .. },
            ) = &phase.actions[i]
            {
                let target = target.clone();
                if is_unmanaged_file(&target, config_dir, state) && !auto_yes {
                    let choice = prompt_backup_choice(&target, None, printer, &options)?;
                    apply_backup_choice(choice, &target, &mut phase.actions[i], printer)?;
                }
            }

            // Module file actions
            if let reconciler::Action::Module(ref ma) = phase.actions[i]
                && let reconciler::ModuleActionKind::DeployFiles { files } = &ma.kind
            {
                let needs_prompt = !auto_yes
                    && files.iter().any(|f| {
                        let t = cfgd_core::expand_tilde(&f.target);
                        is_unmanaged_file(&t, config_dir, state)
                    });
                if needs_prompt {
                    let module_name = ma.module_name.clone();
                    if let reconciler::Action::Module(ref mut ma) = phase.actions[i]
                        && let reconciler::ModuleActionKind::DeployFiles { ref mut files } = ma.kind
                    {
                        let mut j = 0;
                        while j < files.len() {
                            let file_target = cfgd_core::expand_tilde(&files[j].target);
                            if is_unmanaged_file(&file_target, config_dir, state) {
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
            _ => "local".to_string(),
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
/// For package actions, individual packages can be filtered from install/uninstall lists.
pub(in crate::cli) fn filter_plan(plan: &mut reconciler::Plan, skip: &[String], only: &[String]) {
    if skip.is_empty() && only.is_empty() {
        return;
    }

    for phase in &mut plan.phases {
        let mut filtered_actions = Vec::new();

        for action in std::mem::take(&mut phase.actions) {
            // Package install/uninstall actions need per-package granularity
            if let reconciler::Action::Package(ref pa) = action {
                match pa {
                    PackageAction::Install {
                        manager,
                        packages,
                        origin,
                    } => {
                        let kept =
                            filter_package_list(phase.name.as_str(), manager, packages, skip, only);
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
                        let kept =
                            filter_package_list(phase.name.as_str(), manager, packages, skip, only);
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
            let path = action_path(&phase.name, &action);
            let should_skip = skip.iter().any(|s| pattern_matches(s, &path));
            let passes_only = only.is_empty()
                || only
                    .iter()
                    .any(|o| pattern_matches(o, &path) || pattern_matches(&path, o));

            if !should_skip && passes_only {
                filtered_actions.push(action);
            }
        }

        phase.actions = filtered_actions;
    }
}

/// Filter individual packages from an install/uninstall list based on skip/only patterns.
fn filter_package_list(
    phase: &str,
    manager: &str,
    packages: &[String],
    skip: &[String],
    only: &[String],
) -> Vec<String> {
    packages
        .iter()
        .filter(|pkg| {
            let pkg_path = format!("{}.{}.{}", phase, manager, pkg);

            // Check skip: pattern can target the specific package, manager, or phase
            let pkg_skip = skip.iter().any(|s| pattern_matches(s, &pkg_path));

            // Check only: the pattern must cover this package.
            // "packages" covers "packages.brew.ripgrep" (broad → specific)
            // "packages.brew.ripgrep" covers "packages.brew.ripgrep" (exact)
            // But "packages.brew.ripgrep" does NOT cover "packages.brew.fd"
            let pkg_only = only.is_empty()
                || only
                    .iter()
                    .any(|o| pattern_matches(o, &pkg_path) || pattern_matches(&pkg_path, o));

            !pkg_skip && pkg_only
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests;
