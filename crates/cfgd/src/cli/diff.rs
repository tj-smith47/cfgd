use super::*;

use cfgd_core::output::{Doc, OwnerLabel, Printer, Role, TitleLabel, section_guard::SectionGuard};
use cfgd_core::reconciler::{MANAGERS_GROUP, ManagerAction, Owner};

/// Render one module-deployed file's inline diff and report whether it drifts.
///
/// A `Patch` file has no source to compare against — its diff is the target's
/// current content against what re-running the merge would produce — so it
/// routes through the patch renderer while every other strategy keeps the
/// shared source→target renderer.
fn diff_module_file(
    fm: &CfgdFileManager,
    resolved: &cfgd_core::config::ResolvedProfile,
    module: &cfgd_core::modules::ResolvedModule,
    file: &cfgd_core::modules::ResolvedFile,
    config_dir: &std::path::Path,
    strategy: cfgd_core::config::FileStrategy,
    printer: &Printer,
) -> anyhow::Result<cfgd_core::providers::FileDriftResult> {
    match &file.patch {
        Some(spec) => {
            let binding = crate::files::module_patch_binding(config_dir, resolved, module);
            let evaluated =
                cfgd_core::reconciler::evaluate_patch(spec, &file.target, &binding.context());
            Ok(crate::files::render_patch_diff(
                &file.target,
                evaluated,
                printer,
            ))
        }
        // Module sources carry no tera origin, so pass None.
        None => Ok(fm.diff_one(&file.source, &file.target, None, Some(strategy), printer)?),
    }
}

/// Keep only the records worth reporting: a converged file is the absence of a
/// finding, and listing every one of them would bury the drifted and the
/// unevaluable entries a consumer actually acts on.
fn record_file_drift(
    payload: &mut DiffOutput,
    mut record: cfgd_core::providers::FileDriftResult,
    strategy: cfgd_core::config::FileStrategy,
    config_dir: &std::path::Path,
    state: &cfgd_core::state::StateStore,
) -> bool {
    let drifted = !record.matches;
    if drifted {
        // A target cfgd never wrote is a different finding from one that
        // drifted out of cfgd's own content, and the fix for it is a decision
        // rather than a re-write.
        cfgd_core::reconciler::mark_unmanaged_drift(&mut record, strategy, config_dir, state);
        payload.files.push(record);
    }
    drifted
}

/// The owner's group heading, from the one token constructor.
fn owner_label(owner: &Owner) -> OwnerLabel {
    OwnerLabel::new(owner.kind.as_str(), owner.name.as_str())
}

/// The modules a surface walks, by name, so two runs over the same machine
/// report the same findings in the same order.
fn modules_by_name(
    modules: &[cfgd_core::modules::ResolvedModule],
) -> Vec<&cfgd_core::modules::ResolvedModule> {
    let mut sorted: Vec<&cfgd_core::modules::ResolvedModule> = modules.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    sorted
}

/// One module's declared files, by deployment target, for the same reason.
fn files_by_target(
    module: &cfgd_core::modules::ResolvedModule,
) -> Vec<&cfgd_core::modules::ResolvedFile> {
    let mut sorted: Vec<&cfgd_core::modules::ResolvedFile> = module.files.iter().collect();
    sorted.sort_by(|a, b| a.target.cmp(&b.target));
    sorted
}

pub fn cmd_diff(
    cli: &Cli,
    printer: &Printer,
    module_filter: Option<&str>,
    exit_code: bool,
) -> anyhow::Result<()> {
    let ctx = RunContext::new(cli, printer);
    let config_dir = ctx.config_dir();

    if let Some(mod_name) = module_filter {
        printer.heading_title(&TitleLabel::new("Diff", mod_name));
        // The scoped run's header, the same two rows `apply --module` opens
        // on: a title that owns no rows would put its blank line straight
        // under the heading, which no other titled run does.
        let mut rows = cfgd_core::output::config_profile_rows(Some(&cli.config), None);
        rows.push(cfgd_core::output::KvPair::new("Modules", mod_name));
        printer.kv_rows(rows);
        return cmd_diff_module(&ctx, mod_name, exit_code);
    }

    printer.heading("Diff");

    let (cfg, profile_name, local_resolved) = ctx.config_and_profile()?;
    printer.kv_rows(cfgd_core::output::config_profile_rows(
        Some(&cli.config),
        Some(profile_name),
    ));
    // Drift is reported under the same owner that would be named in the plan
    // that fixes it, so the two surfaces read as one coordinate system.
    let profile_owner = Owner::profile(profile_name.to_string());

    // Compose with sources (cache-only — read paths stay offline) and resolve the
    // effective module set through the one shared resolver, so `diff` sees the
    // same source-composed desired state that `apply` writes.
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
    // The registry built from this config and these composed packages, taken
    // before the other fields because a partial move out of `desired` would
    // block the `&mut self` this accessor needs. Its only difference from a
    // `build_registry_with_profile` of the same spec is the config-derived
    // secret backend and default file strategy, neither of which any check
    // below reads.
    let registry = desired.take_registry(cfg);
    let mut resolved = desired.resolved;
    let resolved_modules = desired.modules;

    ctx.resolve_manifest_packages(&mut resolved.merged.packages)?;

    let mut diff_payload = DiffOutput::default();
    let mut has_system_drift = false;

    let file_state = ctx.state()?;
    let has_file_drift = {
        let files_phase = printer.section_or_collapse("Files");
        // The file renderers take a bare `&Printer` and know nothing of the
        // tree; depth inheritance is what lands their per-file lines inside
        // the owner group opened around them.
        let _inherit = printer.depth_inheritance();
        let fm = CfgdFileManager::new(config_dir, &resolved)?;
        let mut drift = false;
        // A finding names its target and nothing else, and whether an unmanaged
        // target is a conflict at all turns on the entry's RESOLVED strategy —
        // the profile-wide default applied once, here, so the profile fold, the
        // module fold and the apply-time sweep cannot disagree about a
        // strategy-less entry.
        let strategies = cfgd_core::effective::effective_file_strategies(
            &resolved.merged,
            &resolved_modules,
            config_dir,
            registry.default_file_strategy,
        );
        {
            let _group = files_phase.section_owner_or_collapse(&owner_label(&profile_owner));
            for record in fm.diff(&resolved.merged, printer)? {
                let strategy = strategies.for_target(&cfgd_core::expand_tilde(
                    std::path::Path::new(&record.target),
                ));
                drift |=
                    record_file_drift(&mut diff_payload, record, strategy, config_dir, file_state);
            }
        }
        // Module-deployed files render the same inline content diff as profile
        // files (module sources carry no tera origin, so pass None).
        for module in modules_by_name(&resolved_modules) {
            let _group =
                files_phase.section_owner_or_collapse(&OwnerLabel::new("module", &module.name));
            for file in files_by_target(module) {
                let strategy = strategies
                    .for_target(&cfgd_core::expand_tilde(std::path::Path::new(&file.target)));
                let record =
                    diff_module_file(&fm, &resolved, module, file, config_dir, strategy, printer)?;
                if record_file_drift(&mut diff_payload, record, strategy, config_dir, file_state) {
                    drift = true;
                }
            }
        }
        drift
    };

    let has_pkg_drift = {
        let pkg_sec = printer.section_or_collapse("Packages");
        let all_managers: Vec<&dyn cfgd_core::providers::PackageManager> = registry
            .package_managers()
            .iter()
            .map(|m| m.as_ref())
            .collect();
        // Tracked-but-dropped packages must surface as drift here, so read the
        // cfgd-installed set from state to bound prune the same way apply does.
        let state = ctx.state()?;
        let cfgd_installed = cfgd_installed_packages(state)?;
        let pkg_cx = cfgd_core::providers::PackageContext::new(printer, state);
        let pkg_actions = packages::plan_packages(
            &resolved.merged,
            &resolved_modules,
            &all_managers,
            &cfgd_installed,
            &pkg_cx,
        )?;
        // Same planner the Prerequisites phase runs, so a manager `diff` calls
        // out as drift is exactly the one `apply` would provision or refuse —
        // and the same predicate `verify`/`status --scan` share via
        // `manager_drift_actions`, so no second membership rule can drift out
        // of sync with the reconciler's.
        let manager_actions: Vec<ManagerAction> = super::live_drift::manager_drift_actions(
            cfgd_core::reconciler::plan_managers(&registry, &pkg_actions, &[]),
        );
        print_package_drift(
            &pkg_actions,
            &manager_actions,
            &pkg_sec,
            &profile_owner,
            &mut diff_payload,
        )
    };

    let has_env_drift = {
        let env_sec = printer.section_or_collapse("Shell");
        let mut drift = false;
        {
            let env_group = env_sec.section_owner_or_collapse(&owner_label(&profile_owner));
            // Must match the recorded bootstrap PATH dirs `cfgd verify` passes: the
            // whole-file check `env_verify_results` bundles in compares against a
            // freshly generated file, and the file cfgd actually wrote carries the
            // bootstrapped PATH export line as its first line. An empty path_dirs
            // here reports permanent, unfixable drift on a machine that bootstrapped
            // any manager.
            let path_dirs = ctx
                .state_opt()
                .map(|state| {
                    cfgd_core::reconciler::recorded_manager_path_dirs(
                        state,
                        &resolved.merged,
                        &resolved_modules,
                    )
                })
                .unwrap_or_default();
            let results = env_drift_ordered(cfgd_core::reconciler::env_verify_results(
                &resolved.merged.env,
                &resolved.merged.aliases,
                &resolved.merged.entry_owners,
                resolved.merged.env_scope,
                &resolved_modules,
                &path_dirs,
            ));
            let drop_env_file_row = cfgd_core::output::env_file_row_is_redundant(
                results.iter().map(|r| r.resource_type.as_str()),
            );
            let merged_env_items = cfgd_core::reconciler::MergedEnvItems::new(
                &resolved.merged.env,
                &resolved.merged.aliases,
                &resolved.merged.entry_owners,
                &resolved_modules,
                &path_dirs,
            );
            for r in results {
                drift = true;
                // An env-var/alias row's `expected`/`actual` are opaque markers —
                // neither real value ever flows into a persisted or gateway-shipped
                // drift record — so recompute both here, for this terminal/`-o json`
                // display only.
                let (expected, actual) = merged_env_items
                    .display_values(&r.resource_type, &r.resource_id)
                    .unwrap_or_else(|| (r.expected.clone(), r.actual.clone()));
                let (expected, actual) =
                    cfgd_core::output::drift_operands(&r.resource_type, &expected, &actual);
                // The payload keeps every finding; only the human report drops
                // the freshness row the item rows beneath it already explain.
                if !(drop_env_file_row && r.resource_type == "env") {
                    env_group
                        .status(
                            Role::Warn,
                            cfgd_core::output::drift_item_subject(&r.resource_type, &r.resource_id),
                        )
                        .drift(&expected, &actual);
                }
                diff_payload.env.push(EnvDriftOutput {
                    kind: r.resource_type,
                    name: r.resource_id,
                    expected,
                    actual,
                });
            }
        }
        drift
    };

    {
        let sys_sec = printer.section_or_collapse("System");
        // Every system key resolves against the merged profile ⊕ module view,
        // which is what puts a system action under the profile owner in the
        // plan too (`owner_of`'s fall-through arm).
        {
            let sys_group = sys_sec.section_owner_or_collapse(&owner_label(&profile_owner));
            let mut available_configurators = registry.available_system_configurators();
            available_configurators.sort_by_key(|c| c.name());
            // Combine profile and module system config so module system tweaks
            // surface in `diff` exactly as they do on the write path.
            let system =
                cfgd_core::effective::effective_system_map(&resolved.merged, &resolved_modules);
            for configurator in &available_configurators {
                let key = configurator.name();
                let desired = match system.get(key) {
                    Some(v) => v,
                    None => continue,
                };
                match configurator.diff(desired) {
                    Ok(mut drifts) if !drifts.is_empty() => {
                        has_system_drift = true;
                        drifts.sort_by(|a, b| a.key.cmp(&b.key));
                        for drift in &drifts {
                            // A configurator that found no setting at all hands
                            // back an empty `actual`; the fold states the
                            // absence instead of rendering `have: `.
                            let (expected, actual) = cfgd_core::output::drift_operands(
                                "system",
                                &drift.expected,
                                &drift.actual,
                            );
                            sys_group
                                .status(Role::Warn, format!("{}.{}", key, drift.key))
                                .drift(&expected, &actual);
                            diff_payload.system.push(SystemDriftOutput {
                                key: format!("{}.{}", key, drift.key),
                                expected: drift.expected.clone(),
                                actual: drift.actual.clone(),
                            });
                        }
                    }
                    Err(e) => {
                        let error = cfgd_core::output::collapse_to_subject_line(e);
                        sys_group
                            .status(Role::Warn, key.to_string())
                            .qualifier("error checking drift")
                            .detail(&error);
                        diff_payload.system_errors.push(SystemCheckError {
                            key: key.to_string(),
                            error,
                        });
                    }
                    _ => {}
                }
            }
        }
    }

    diff_payload.summary = DiffSummary {
        has_file_drift,
        has_pkg_drift,
        has_system_drift,
        system_check_failed: !diff_payload.system_errors.is_empty(),
        has_env_drift,
        // The full desired state this path needs for every phase was already
        // resolved above (line ~95) before any section opened; a failure
        // there aborts the whole command via `?`, so this path never reaches
        // here with the env check unresolved.
        env_check_failed: false,
    };

    // This command just checked the machine itself, whatever it found — the
    // recorded-state `status` header dates its display from here, and a scan
    // that finds nothing is exactly the one a clean host has no other record of.
    if let Ok(state) = ctx.state() {
        state.record_scan();
    }

    printer.emit(build_diff_doc(&diff_payload, DiffScope::Machine));

    if exit_code && let Some(code) = diff_exit_code(&diff_payload.summary) {
        code.exit();
    }

    Ok(())
}

/// Env findings in the order they render: by kind, then by the item named.
///
/// `env_verify_results` answers in check order (the managed file and its rc
/// lines, then each declared item), which says nothing about the machine — two
/// runs finding the same drift must read the same rather than reordering by
/// whatever the check reached first.
fn env_drift_ordered(
    results: Vec<cfgd_core::reconciler::VerifyResult>,
) -> Vec<cfgd_core::reconciler::VerifyResult> {
    let mut drifted: Vec<_> = results.into_iter().filter(|r| !r.matches).collect();
    drifted.sort_by(|a, b| {
        a.resource_type
            .cmp(&b.resource_type)
            .then_with(|| a.resource_id.cmp(&b.resource_id))
    });
    drifted
}

/// What `--exit-code` reports, from the same summary the `-o json` payload
/// carries — so the exit status and the payload can never disagree about
/// whether this machine is in sync.
///
/// A failed check outranks drift: `DriftDetected` tells a script the machine
/// needs an apply, while a check that could not run means the answer is
/// unknown, which is an error rather than a verdict.
fn diff_exit_code(summary: &DiffSummary) -> Option<cfgd_core::exit::ExitCode> {
    if summary.system_check_failed || summary.env_check_failed {
        return Some(cfgd_core::exit::ExitCode::Error);
    }
    (summary.has_file_drift
        || summary.has_pkg_drift
        || summary.has_system_drift
        || summary.has_env_drift)
        .then_some(cfgd_core::exit::ExitCode::DriftDetected)
}

fn cmd_diff_module(ctx: &RunContext<'_>, mod_name: &str, exit_code: bool) -> anyhow::Result<()> {
    let cli = ctx.cli();
    let printer = ctx.printer();
    let config_dir = ctx.config_dir();
    let registry = ctx.base_registry();
    let platform = Platform::current();
    let mgr_map = registry.manager_map();
    let cache_base = module_cache_dir(cli)?;
    let resolved_modules = match modules::resolve_modules(
        &[mod_name.to_string()],
        config_dir,
        &cache_base,
        &[],
        platform,
        &mgr_map,
        printer,
    ) {
        Ok(mods) => mods,
        // "not found" is reserved for a genuinely unknown module name; any
        // other resolution failure (e.g. a dependency cycle among local
        // modules) must surface as the error it is, not read as a miss. This
        // call passes an empty `source_roots`, so `ScriptsNotAllowed` can
        // never originate here — that constraint is enforced only where a
        // source's own module roots are resolved (`resolve_desired_state`).
        Err(e)
            if matches!(
                &e,
                cfgd_core::errors::CfgdError::Module(
                    cfgd_core::errors::ModuleError::NotFound { .. }
                )
            ) =>
        {
            printer.emit(
                Doc::new()
                    .status(
                        Role::Info,
                        format!(
                            "Module '{}' {} — nothing to diff",
                            mod_name,
                            cfgd_core::Absence::NotFound
                        ),
                    )
                    .with_data(DiffOutput::default()),
            );
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };

    let state = ctx.state()?;
    let pkg_cx = cfgd_core::providers::PackageContext::new(printer, state);

    let mut diff_payload = DiffOutput::default();
    let mut has_file_diff = false;
    let mut has_pkg_drift = false;

    {
        // Mirror the full `cmd_diff` path: the `Files` heading, one
        // `module:<name>` group per module, the shared per-file inline-diff
        // renderer. Module sources carry no tera origin (None).
        let files_phase = printer.section_or_collapse("Files");
        let _inherit = printer.depth_inheritance();
        let resolved = empty_resolved_profile(&[mod_name.to_string()], &ctx.active_profile_name());
        let fm = CfgdFileManager::new(config_dir, &resolved)?;
        // The RESOLVED strategy, so a module entry declaring none is judged
        // against the same profile-wide default the full `diff` path and the
        // apply-time sweep read it against.
        let strategies = cfgd_core::effective::effective_file_strategies(
            &resolved.merged,
            &resolved_modules,
            config_dir,
            ctx.base_registry().default_file_strategy,
        );
        for module in modules_by_name(&resolved_modules) {
            let _group =
                files_phase.section_owner_or_collapse(&OwnerLabel::new("module", &module.name));
            for file in files_by_target(module) {
                let strategy = strategies
                    .for_target(&cfgd_core::expand_tilde(std::path::Path::new(&file.target)));
                let record =
                    diff_module_file(&fm, &resolved, module, file, config_dir, strategy, printer)?;
                if record_file_drift(&mut diff_payload, record, strategy, config_dir, state) {
                    has_file_diff = true;
                }
            }
        }
    }

    {
        let pkg_sec = printer.section_or_collapse("Packages");
        for module in modules_by_name(&resolved_modules) {
            let group = pkg_sec.section_owner_or_collapse(&OwnerLabel::new("module", &module.name));
            let mut packages: Vec<&modules::ResolvedPackage> = module.packages.iter().collect();
            packages.sort_by(|a, b| a.resolved_name.cmp(&b.resolved_name));
            for pkg in packages {
                if let Some(drift) = package_missing_drift(pkg, &mgr_map, &pkg_cx) {
                    has_pkg_drift = true;
                    group
                        .status(Role::Warn, pkg.manager.clone())
                        .qualifier(cfgd_core::Absence::NotInstalled.as_str())
                        .detail(pkg.resolved_name.clone());
                    diff_payload.packages.push(drift);
                }
            }
        }
    }

    // The managed env file and rc source line are the ACTIVE PROFILE's shared
    // artifacts, not this module's own: the planner wrote them from the whole
    // profile plus every module, so checking them against one module's
    // fragment — and a hardcoded EnvScope::All — reports permanent stale
    // drift on a fully converged machine. Resolve the same full desired
    // state the non-`--module` path checks against, so `verify` / `diff` /
    // `diff --module` all agree about one machine. Resolved BEFORE any
    // section opens: `resolve_desired_state` performs its own top-level
    // Printer emits (module-resolution status, git-fetch warnings), and
    // nesting that call inside an already-open `SectionGuard` tripped the
    // renderer's structural-depth guard and mis-indented that output under
    // the Shell section instead of at the top level.
    let (cfg, profile_name, local_resolved) = ctx.config_and_profile()?;
    let full_desired_result = resolve_desired_state(
        ctx,
        cfg,
        local_resolved,
        &[],
        false,
        printer,
        false,
        composition::ConstraintMode::Report,
    );

    // `env_sec` must drop before `build_diff_doc`'s top-level emit below: a
    // `SectionGuard` still in scope there is exactly the mis-nesting this
    // whole reorder exists to avoid, so its lifetime is bounded to this block
    // rather than left open to the end of the function.
    let (has_env_drift, env_check_failed) = {
        let env_sec = printer.section_or_collapse("Shell");
        match full_desired_result {
            // A module UNRELATED to the one this run was scoped to (a different
            // module in the same profile, failing to resolve) must not discard
            // the Files/Packages diff already computed above for the module that
            // was actually asked about — report the env check as undetermined
            // rather than aborting the whole `--module` diff.
            Err(e) => {
                let error = cfgd_core::output::collapse_to_subject_line(e);
                env_sec
                    // name-row-ok: the row names the profile surface, not an outcome
                    .status(Role::Warn, "profile")
                    .qualifier("could not resolve the active profile")
                    .detail(&error);
                diff_payload.env_check_error = Some(error);
                (false, true)
            }
            Ok(full_desired) => {
                let mut drift = false;
                let full_resolved = &full_desired.resolved;
                let path_dirs = cfgd_core::reconciler::recorded_manager_path_dirs(
                    state,
                    &full_resolved.merged,
                    &full_desired.modules,
                );
                let all_results = env_drift_ordered(cfgd_core::reconciler::env_verify_results(
                    &full_resolved.merged.env,
                    &full_resolved.merged.aliases,
                    &full_resolved.merged.entry_owners,
                    full_resolved.merged.env_scope,
                    &full_desired.modules,
                    &path_dirs,
                ));

                // Judged over the item rows this run will actually RENDER: a
                // file made stale by a profile-level entry no resolved module
                // owns has no item row to explain it, so its freshness row
                // still stands.
                let rendered_items: std::collections::HashSet<&str> = resolved_modules
                    .iter()
                    .flat_map(|m| {
                        m.env
                            .iter()
                            .map(|e| e.name.as_str())
                            .chain(m.aliases.iter().map(|a| a.name.as_str()))
                    })
                    .collect();
                let drop_env_file_row = cfgd_core::output::env_file_row_is_redundant(
                    all_results
                        .iter()
                        .filter(|r| rendered_items.contains(r.resource_id.as_str()))
                        .map(|r| r.resource_type.as_str()),
                );

                // The file/rc rows are the profile's shared artifacts, not any one
                // module's — report them once, under the profile itself, whichever
                // declared item made the file stale.
                {
                    let group = env_sec.section_owner_or_collapse(&owner_label(&Owner::profile(
                        profile_name.to_string(),
                    )));
                    for r in all_results
                        .iter()
                        .filter(|r| matches!(r.resource_type.as_str(), "env" | "env-rc"))
                    {
                        drift = true;
                        let (expected, actual) = cfgd_core::output::drift_operands(
                            &r.resource_type,
                            &r.expected,
                            &r.actual,
                        );
                        if !(drop_env_file_row && r.resource_type == "env") {
                            group
                                .status(
                                    Role::Warn,
                                    cfgd_core::output::drift_item_subject(
                                        &r.resource_type,
                                        &r.resource_id,
                                    ),
                                )
                                .drift(&expected, &actual);
                        }
                        diff_payload.env.push(EnvDriftOutput {
                            kind: r.resource_type.clone(),
                            name: r.resource_id.clone(),
                            expected,
                            actual,
                        });
                    }
                }

                // Per-item rows are attributed to whichever of this run's own resolved
                // modules (the target plus its dependencies) declares them.
                let merged_env_items = cfgd_core::reconciler::MergedEnvItems::new(
                    &full_resolved.merged.env,
                    &full_resolved.merged.aliases,
                    &full_resolved.merged.entry_owners,
                    &resolved_modules,
                    &path_dirs,
                );
                for module in modules_by_name(&resolved_modules) {
                    let owns: std::collections::HashSet<&str> = module
                        .env
                        .iter()
                        .map(|e| e.name.as_str())
                        .chain(module.aliases.iter().map(|a| a.name.as_str()))
                        .collect();
                    let group =
                        env_sec.section_owner_or_collapse(&OwnerLabel::new("module", &module.name));
                    for r in all_results.iter().filter(|r| {
                        matches!(r.resource_type.as_str(), "env-var" | "alias")
                            && owns.contains(r.resource_id.as_str())
                    }) {
                        drift = true;
                        // Opaque markers carry neither real value — recompute both
                        // here, for this terminal/`-o json` display only.
                        let (expected, actual) = merged_env_items
                            .display_values(&r.resource_type, &r.resource_id)
                            .unwrap_or_else(|| (r.expected.clone(), r.actual.clone()));
                        let (expected, actual) =
                            cfgd_core::output::drift_operands(&r.resource_type, &expected, &actual);
                        group
                            .status(
                                Role::Warn,
                                cfgd_core::output::drift_item_subject(
                                    &r.resource_type,
                                    &r.resource_id,
                                ),
                            )
                            .drift(&expected, &actual);
                        diff_payload.env.push(EnvDriftOutput {
                            kind: r.resource_type.clone(),
                            name: r.resource_id.clone(),
                            expected,
                            actual,
                        });
                    }
                }

                (drift, false)
            }
        }
    };

    diff_payload.summary = DiffSummary {
        has_file_drift: has_file_diff,
        has_pkg_drift,
        // A single module's diff evaluates no system configurator, so the
        // system verdict is neither drifted nor undetermined here.
        has_system_drift: false,
        system_check_failed: false,
        has_env_drift,
        env_check_failed,
    };

    printer.emit(build_diff_doc(&diff_payload, DiffScope::Module(mod_name)));

    if exit_code && let Some(code) = diff_exit_code(&diff_payload.summary) {
        code.exit();
    }

    Ok(())
}

/// The ONE grammar for a package drift's `resource_id`: `<manager>:<packages,
/// comma-joined>`. Three call sites mint this string — `package_action_drift`
/// and `resolve_module_files_and_packages` (both in `live_drift.rs`) and
/// `cmd_status_module`'s missing-package branch (`status.rs`) — and a
/// consumer diffing two `DriftEvent`/`VerifyResult` streams needs all three to
/// agree on the separator. `/` collides with a package name that legitimately
/// contains one (a scoped npm package, `@org/name`), which `:` cannot.
pub(super) fn package_resource_id(manager: &str, packages: &[String]) -> String {
    format!("{}:{}", manager, packages.join(", "))
}

/// Drift record for a module-declared package that is not installed, or `None`
/// when it is installed, script-based, or its manager isn't registered.
/// The comparison routes through `package_identity` so case-insensitive managers
/// (choco/scoop/winget) and name-remapping ones (go) match installed state like
/// with like — a raw name compare re-reports installed packages as missing on
/// every `cfgd diff --module`.
///
/// Called once per declared package by both `cfgd diff --module` and `cfgd
/// status --module -e`, so the installed set comes from `cx`'s memo: a module
/// with N packages on one manager asks that manager once, not N times.
pub(super) fn package_missing_drift(
    pkg: &modules::ResolvedPackage,
    mgr_map: &std::collections::HashMap<String, &dyn cfgd_core::providers::PackageManager>,
    cx: &cfgd_core::providers::PackageContext<'_>,
) -> Option<PackageDrift> {
    if pkg.manager == "script" {
        return None;
    }
    let mgr = mgr_map.get(pkg.manager.as_str())?;
    // A manager that cannot be queried leaves the package reported missing,
    // exactly as an empty enumeration did.
    let installed = cx.installed_for(*mgr).ok();
    if installed.is_some_and(|set| set.contains(&mgr.package_identity(&pkg.resolved_name))) {
        return None;
    }
    Some(PackageDrift {
        manager: pkg.manager.clone(),
        shape: cfgd_core::Absence::Missing.to_string(),
        packages: vec![pkg.resolved_name.clone()],
        bootstrap_method: None,
        reason: None,
    })
}

/// Render the package half of a drift report, one owner group per owner.
///
/// `manager_actions` is the same `ManagerAction` planner output the
/// Prerequisites phase runs (`reconciler::plan_managers`) — a missing manager
/// this run would provision, or refuses to, is drift the same way a missing
/// package is, and reads under `cfgd:managers` exactly as it would in the
/// plan that fixes it. `RefreshIndex`/`Prerequisite` nodes are not drift (an
/// index refresh and a tool install are not something the user declared and
/// can be missing) and never reach this function's caller.
pub(super) fn print_package_drift(
    pkg_actions: &[PackageAction],
    manager_actions: &[ManagerAction],
    section: &SectionGuard<'_>,
    profile: &Owner,
    payload: &mut DiffOutput,
) -> bool {
    let mut pkg_diffs: Vec<&PackageAction> = pkg_actions
        .iter()
        .filter(|a| !matches!(a, PackageAction::Skip { .. }))
        .collect();
    // Planner order is whatever the dependency walk produced; the report reads
    // the same for one machine however that walk was reached.
    pkg_diffs.sort_by_key(|a| match a {
        PackageAction::Install {
            manager, packages, ..
        }
        | PackageAction::Uninstall {
            manager, packages, ..
        } => (manager.clone(), packages.join(", ")),
        PackageAction::Skip { manager, .. } => (manager.clone(), String::new()),
    });
    let has_drift = !pkg_diffs.is_empty() || !manager_actions.is_empty();
    if !has_drift {
        return false;
    }
    let managers_owner = Owner::cfgd(MANAGERS_GROUP);
    let mut owners: Vec<Owner> = Vec::new();
    if !pkg_diffs.is_empty() {
        owners.push(profile.clone());
    }
    if !manager_actions.is_empty() {
        owners.push(managers_owner.clone());
    }
    Owner::order(&mut owners);
    for owner in &owners {
        let group = section.section_owner(&owner_label(owner));
        if *owner == managers_owner {
            for ma in manager_actions {
                // The line's words come from the one derivation `verify` and
                // `status --scan` fold into their own rows, so the two surfaces
                // cannot describe one unprovisionable manager two ways.
                let Some(phrase) = super::live_drift::manager_drift_phrase(ma) else {
                    continue;
                };
                match ma {
                    // One line and one payload row per manager the node
                    // provisions: a batch installs several from one command,
                    // and every one of them is missing on its own terms.
                    ManagerAction::Provision { via, .. } => {
                        for manager in ma.provisioned_managers() {
                            group
                                .status(Role::Warn, manager)
                                .qualifier(phrase.state)
                                .detail(phrase.detail.clone());
                            payload.packages.push(PackageDrift {
                                manager: manager.to_string(),
                                shape: "provision".to_string(),
                                packages: Vec::new(),
                                bootstrap_method: Some(via.clone()),
                                reason: None,
                            });
                        }
                    }
                    ManagerAction::Refuse { manager, reason } => {
                        group
                            .status(Role::Warn, manager.clone())
                            .qualifier(phrase.state)
                            .detail(phrase.detail.clone());
                        payload.packages.push(PackageDrift {
                            manager: manager.clone(),
                            shape: "refused".to_string(),
                            packages: Vec::new(),
                            bootstrap_method: None,
                            reason: Some(reason.clone()),
                        });
                    }
                    ManagerAction::RefreshIndex { .. } | ManagerAction::Prerequisite { .. } => {}
                }
            }
            continue;
        }
        for action in &pkg_diffs {
            match action {
                PackageAction::Install {
                    manager, packages, ..
                } => {
                    group
                        .status(Role::Warn, manager.clone())
                        .qualifier(cfgd_core::Absence::NotInstalled.as_str())
                        .detail(packages.join(", "));
                    payload.packages.push(PackageDrift {
                        manager: manager.clone(),
                        // The persisted/`-o json` shape stays the literal it
                        // has always been; only the rendered qualifier moves
                        // to the one absence word every surface now uses.
                        shape: cfgd_core::Absence::Missing.to_string(),
                        packages: packages.clone(),
                        bootstrap_method: None,
                        reason: None,
                    });
                }
                PackageAction::Uninstall {
                    manager, packages, ..
                } => {
                    group
                        .status(Role::Warn, manager.clone())
                        .qualifier("extra")
                        .detail(packages.join(", "));
                    payload.packages.push(PackageDrift {
                        manager: manager.clone(),
                        shape: "extra".to_string(),
                        packages: packages.clone(),
                        bootstrap_method: None,
                        reason: None,
                    });
                }
                PackageAction::Skip { .. } => {}
            }
        }
    }
    has_drift
}

/// How much of the machine a diff run looked at.
///
/// `--module` evaluates no system configurator, so its closing line must not
/// call `system` clean — that would claim a check the run never made. Nothing
/// in [`DiffSummary`] can tell the two apart: a module run and a machine run
/// with no `spec.system` both report `has_system_drift: false`.
/// The module name rides along because the report's closing next step is
/// scoped the way the report was: a `--module` run that found drift heals with
/// `cfgd apply --module <name>`, not with a whole-machine apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffScope<'a> {
    Machine,
    Module(&'a str),
}

/// The closing line's tally: what drifted, and which of the surfaces this run
/// checked came back clean (`1 file (packages, shell clean)`).
///
/// A surface whose check could not RUN is named by neither half — the same
/// line already carries the reason it could not, and calling it clean or
/// drifted would both be claims the run cannot make.
fn drift_tally(output: &DiffOutput, scope: DiffScope<'_>) -> String {
    let s = &output.summary;
    // The payload keeps every finding; the REPORT drops the env file's own
    // freshness row when the item rows beneath it already explain it. The
    // tally counts what the reader can see, so a report showing one row cannot
    // close by charging for two — the same predicate the render applied, read
    // back off the payload the render filled.
    let shell_rows =
        if cfgd_core::output::env_file_row_is_redundant(output.env.iter().map(|e| e.kind.as_str()))
        {
            output.env.iter().filter(|e| e.kind != "env").count()
        } else {
            output.env.len()
        };
    let surfaces = [
        ("files", "file", output.files.len(), s.has_file_drift, true),
        (
            "packages",
            "package",
            output.packages.len(),
            s.has_pkg_drift,
            true,
        ),
        (
            "shell",
            "shell item",
            shell_rows,
            s.has_env_drift,
            !s.env_check_failed,
        ),
        (
            "system",
            "system setting",
            output.system.len(),
            s.has_system_drift,
            scope == DiffScope::Machine && !s.system_check_failed,
        ),
    ];

    let mut drifted = Vec::new();
    let mut clean = Vec::new();
    for (label, noun, count, has_drift, decided) in surfaces {
        if has_drift {
            drifted.push(cfgd_core::pluralize(count, noun));
        } else if decided {
            clean.push(label);
        }
    }
    let drifted = drifted.join(", ");
    if clean.is_empty() {
        drifted
    } else {
        format!("{drifted} ({} clean)", clean.join(", "))
    }
}

pub fn build_diff_doc(output: &DiffOutput, scope: DiffScope<'_>) -> Doc {
    let any_drift = output.summary.has_file_drift
        || output.summary.has_pkg_drift
        || output.summary.has_system_drift
        || output.summary.has_env_drift;
    // A run that could not check everything has no clean verdict to give, so
    // it never renders one — whether or not the checks that DID run found
    // drift.
    let check_failed = output.summary.system_check_failed || output.summary.env_check_failed;
    let role = if any_drift || check_failed {
        Role::Warn
    } else {
        Role::Ok
    };
    let mut reasons = Vec::new();
    if output.summary.system_check_failed {
        reasons.push("a system check could not run".to_string());
    }
    if output.summary.env_check_failed {
        reasons.push("the shell check could not run".to_string());
    }
    if any_drift {
        // Drift AND a check that could not run: the verdict names the drift,
        // but the exit code is `Error` rather than `DriftDetected`, and the
        // reason for it is owed to whoever reads the two together.
        let detail = std::iter::once(drift_tally(output, scope))
            .chain(reasons)
            .collect::<Vec<_>>()
            .join("; ");
        return Doc::new()
            .status_with(role, "Drift detected", |f| f.detail(detail))
            .hint(super::heal_drift_hint(match scope {
                DiffScope::Machine => None,
                DiffScope::Module(name) => Some(name),
            }))
            .with_data(output);
    }
    if check_failed {
        return Doc::new()
            .status_with(role, "Drift undetermined", |f| f.detail(reasons.join("; ")))
            .with_data(output);
    }
    Doc::new()
        .status(role, "No drift detected")
        .with_data(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    use serial_test::serial;

    /// `cfgd diff --module <name>` against a local module set carrying a
    /// dependency cycle must surface the typed `ModuleError::DependencyCycle`
    /// — the real reason resolution failed — rather than the "not found"
    /// render `cmd_diff_module`'s `Err(e) if matches!(.., ModuleError::
    /// NotFound { .. })` arm reserves for a genuinely unknown module name. A
    /// prior revision matched on any `Err(_)` here and silently rendered
    /// "not found" for every resolution failure alike, which is exactly what
    /// this pins against regressing. `resolve_modules` is called with an
    /// empty `source_roots` at this call site (see the comment above the
    /// match), so a dependency cycle — not `ScriptsNotAllowed`, which needs a
    /// source's own module roots — is the reachable non-`NotFound` error here.
    #[test]
    #[serial]
    fn cmd_diff_module_dependency_cycle_surfaces_the_real_error() {
        use crate::cli::helpers::tests::{make_cli, quiet_printer};

        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n",
        )
        .unwrap();
        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(
            profiles_dir.join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
        )
        .unwrap();

        let modules_dir = tmp.path().join("modules");
        for (name, dependency) in [("cycle-a", "cycle-b"), ("cycle-b", "cycle-a")] {
            let dir = modules_dir.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("module.yaml"),
                format!(
                    "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: {name}\nspec:\n  depends: [{dependency}]\n"
                ),
            )
            .unwrap();
        }

        let mut cli = make_cli(config_path);
        cli.state_dir = Some(tmp.path().join("state"));
        cli.cache_dir = Some(tmp.path().join("cache"));
        let printer = quiet_printer();

        let err = cmd_diff(&cli, &printer, Some("cycle-a"), false).unwrap_err();
        let cfgd_err = err
            .downcast_ref::<cfgd_core::errors::CfgdError>()
            .unwrap_or_else(|| panic!("expected a typed CfgdError, got: {err}"));
        assert!(
            matches!(
                cfgd_err,
                cfgd_core::errors::CfgdError::Module(
                    cfgd_core::errors::ModuleError::DependencyCycle { .. }
                )
            ),
            "expected DependencyCycle, got: {cfgd_err}"
        );
    }

    /// `cfgd diff`'s new Env phase must surface a hand-edited alias as drift on
    /// every channel: the human phase line, the per-item status row, the
    /// `-o json` `env` array and the top-level drift verdict fields
    /// (`build_diff_doc`'s `any_drift` and `diff_exit_code`'s gate). Both were
    /// found missing `has_env_drift` while wiring this phase in, and neither
    /// is covered by a test that drives a real `.cfgd.env` mismatch through
    /// the full command rather than a hand-built `DiffOutput`.
    #[test]
    #[serial]
    fn cmd_diff_reports_a_hand_edited_alias_as_env_drift() {
        use crate::cli::helpers::tests::make_cli;

        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n",
        )
        .unwrap();
        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(
            profiles_dir.join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  aliases:\n    - name: ll\n      command: ls -la\n",
        )
        .unwrap();

        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        // The primary managed env file's dialect is platform-dependent (bash
        // `alias` on Unix, a PowerShell `function`/`Set-Alias` on Windows), so
        // the hand-edited line is derived from `env_item_declared_line` —
        // production's own per-item renderer for the running platform —
        // rather than a hardcoded POSIX literal.
        let hand_edited = cfgd_core::config::ShellAlias {
            name: "ll".to_string(),
            command: "ls -lah".to_string(),
            platforms: vec![],
        };
        let hand_edited_line = cfgd_core::reconciler::MergedEnvItems::new(
            &[],
            std::slice::from_ref(&hand_edited),
            &Default::default(),
            &[],
            &[],
        )
        .declared_line("alias", "ll")
        .expect("alias renders a declared line");
        std::fs::write(
            cfgd_core::reconciler::primary_env_file(tmp_home.path()),
            format!("# managed by cfgd \u{2014} do not edit\n{hand_edited_line}\n"),
        )
        .unwrap();

        let mut cli = make_cli(config_path);
        cli.state_dir = Some(tmp.path().join("state"));
        cli.cache_dir = Some(tmp.path().join("cache"));
        let (printer, buf) = Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

        cmd_diff(&cli, &printer, None, false).unwrap();
        drop(printer);
        let human = strip_ansi(&cfgd_core::test_helpers::captured_text(&buf));
        assert!(
            human.contains("\nShell\n"),
            "the Shell surface must render, since it has a finding: {human}"
        );
        assert!(
            human.contains("alias: ll"),
            "the drifted alias must be named: {human}"
        );
        // The row names both real lines: what the declaration renders as, and
        // what the file was hand-edited to hold. Pinned against production's
        // own renderer and the exact bytes the fixture wrote, so a regression
        // back to an opaque `have` marker fails here.
        let declared = cfgd_core::config::ShellAlias {
            name: "ll".to_string(),
            command: "ls -la".to_string(),
            platforms: vec![],
        };
        // The owners the profile-layer merge records: the declared line names
        // the layer that declared it, so a needle rendered with no owner is a
        // line production never renders.
        let declared_owners = {
            let mut o = cfgd_core::config::EntryOwners::default();
            o.claim("profile:default", &[], std::slice::from_ref(&declared));
            o
        };
        let declared_line = cfgd_core::reconciler::MergedEnvItems::new(
            &[],
            std::slice::from_ref(&declared),
            &declared_owners,
            &[],
            &[],
        )
        .declared_line("alias", "ll")
        .expect("alias renders a declared line");
        assert!(
            human.contains(&cfgd_core::output::drift_detail(
                &declared_line,
                &hand_edited_line
            )),
            "the row must show the declared line against the line on disk: {human}"
        );
    }

    /// The clean-case counterpart: a `.cfgd.env` that already holds the
    /// declared alias's exact line reports no drift FOR THAT ALIAS, on both
    /// the human row and the `-o json` `env` array. The fixture's home
    /// directory carries no rc files or `environment.d` entry of its own, so
    /// this only pins the per-item alias/env-var check — the whole-file and
    /// source-line checks the other env targets exercise are covered by
    /// their own tests and are free to report their own unrelated drift
    /// here.
    #[test]
    #[serial]
    fn cmd_diff_reports_no_env_drift_when_alias_line_matches() {
        use crate::cli::helpers::tests::make_cli;

        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n",
        )
        .unwrap();
        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(
            profiles_dir.join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  aliases:\n    - name: ll\n      command: ls -la\n",
        )
        .unwrap();

        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        std::fs::write(
            tmp_home.path().join(".cfgd.env"),
            "# managed by cfgd \u{2014} do not edit\nalias ll=\"ls -la\" # profile:default\n",
        )
        .unwrap();

        let mut cli = make_cli(config_path);
        cli.state_dir = Some(tmp.path().join("state"));
        cli.cache_dir = Some(tmp.path().join("cache"));
        let (printer, cap) = Printer::for_test_doc();

        cmd_diff(&cli, &printer, None, false).unwrap();
        drop(printer);
        let human = strip_ansi(&cap.human());
        assert!(
            !human.contains("alias: ll"),
            "a matching alias line must not be reported as drift: {human}"
        );
        let json = cap.json().expect("diff emits a data payload");
        let env = json["env"].as_array().expect("env array present");
        assert!(
            !env.iter()
                .any(|e| e["kind"] == "alias" && e["name"] == "ll"),
            "a matching alias must not appear in the env drift payload: {env:?}"
        );
    }

    /// Blocker regression: `cmd_diff` must pass the SAME recorded bootstrap
    /// PATH dirs `cfgd verify` persists, or the managed env file's whole-file
    /// check compares against generated content missing the `export PATH=…`
    /// line the file actually carries — permanent, unfixable "stale" drift on
    /// a fully converged machine that bootstrapped any manager. A profile
    /// declaring a `chocolatey` package plus a state store recording
    /// `chocolatey`'s bootstrap PATH dir reproduces exactly that machine: an
    /// `.cfgd.env` written byte-for-byte with the recorded PATH line must
    /// report no `env` drift for that file. `chocolatey` is deliberately a
    /// Windows-only manager — its `is_available()`/`can_bootstrap()` are
    /// platform-gated false on this Linux test host, so package planning
    /// never shells out to a real package manager; only the declared name
    /// needs to reach `effective_desired_packages` for
    /// `recorded_manager_path_dirs` to pick up its recorded dir.
    #[test]
    #[serial]
    fn cmd_diff_reports_no_env_drift_when_bootstrap_path_dirs_are_recorded() {
        use crate::cli::helpers::tests::make_cli;

        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n",
        )
        .unwrap();
        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(
            profiles_dir.join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  envScope: Interactive\n  packages:\n    chocolatey:\n      - black\n",
        )
        .unwrap();

        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        // Byte-for-byte what the generator produces from the recorded
        // bootstrap dir: header, then the PATH export line naming the manager
        // that bootstrapped it, no declared env vars or aliases.
        std::fs::write(
            tmp_home.path().join(".cfgd.env"),
            "# managed by cfgd \u{2014} do not edit\nexport PATH=\"/opt/choco/bin:$PATH\" # manager:chocolatey\n",
        )
        .unwrap();

        let state_dir = tmp.path().join("state");
        let mut cli = make_cli(config_path);
        cli.state_dir = Some(state_dir.clone());
        cli.cache_dir = Some(tmp.path().join("cache"));

        // The same state a real `cfgd verify` (or an apply that bootstrapped
        // chocolatey) would have recorded.
        let state = open_state_store(Some(&state_dir), cfgd_core::Scope::User).unwrap();
        state
            .record_bootstrapped_path_dirs("chocolatey", &["/opt/choco/bin".to_string()])
            .unwrap();
        drop(state);

        let (printer, cap) = Printer::for_test_doc();
        cmd_diff(&cli, &printer, None, false).unwrap();
        drop(printer);

        let json = cap.json().expect("diff emits a data payload");
        let env = json["env"].as_array().expect("env array present");
        let env_file_path = cfgd_core::to_posix_string(tmp_home.path().join(".cfgd.env"));
        assert!(
            !env.iter()
                .any(|e| e["kind"] == "env" && e["name"] == env_file_path.as_str()),
            "the managed env file must not report stale once diff passes the \
             same recorded bootstrap PATH dirs verify does: {env:?}"
        );
    }

    /// Blocker regression: `cfgd diff --module <name>` must check the shared
    /// managed env file against the FULL desired state (profile plus every
    /// module), not against the target module's own fragment. A profile
    /// declaring `PAGER` directly plus a module declaring `EDITOR` reproduces
    /// two declarations that only combine at the profile level: an
    /// `.cfgd.env` written with BOTH lines is what a real `cfgd apply` would
    /// leave behind, and diffing just the module in isolation must not read
    /// the other declaration's absence as file-level drift.
    #[test]
    #[serial]
    fn cmd_diff_module_does_not_report_the_shared_env_file_stale_against_its_own_fragment() {
        use crate::cli::helpers::tests::make_cli;

        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n",
        )
        .unwrap();
        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(
            profiles_dir.join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  envScope: Interactive\n  env:\n    - name: PAGER\n      value: less\n  modules:\n    - env-mod\n",
        )
        .unwrap();
        let mod_dir = tmp.path().join("modules").join("env-mod");
        std::fs::create_dir_all(mod_dir.join("files")).unwrap();
        std::fs::write(
            mod_dir.join("module.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: env-mod\nspec:\n  env:\n    - name: EDITOR\n      value: vim\n",
        )
        .unwrap();

        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        // Byte-for-byte what `generate_env_file_content` produces from the
        // FULL merged env — profile vars first, then each module's, per
        // `merge_module_env_aliases` — so `PAGER` (profile) precedes
        // `EDITOR` (module): exactly what a real apply of this profile would
        // have written.
        std::fs::write(
            tmp_home.path().join(".cfgd.env"),
            "# managed by cfgd \u{2014} do not edit\nexport PAGER=\"less\" # profile:default\nexport EDITOR=\"vim\" # module:env-mod\n",
        )
        .unwrap();

        let mut cli = make_cli(config_path);
        cli.state_dir = Some(tmp.path().join("state"));
        cli.cache_dir = Some(tmp.path().join("cache"));
        let (printer, cap) = Printer::for_test_doc();

        cmd_diff(&cli, &printer, Some("env-mod"), false).unwrap();
        drop(printer);

        let json = cap.json().expect("diff emits a data payload");
        let env = json["env"].as_array().expect("env array present");
        let env_file_path = cfgd_core::to_posix_string(tmp_home.path().join(".cfgd.env"));
        assert!(
            !env.iter()
                .any(|e| e["kind"] == "env" && e["name"] == env_file_path.as_str()),
            "diffing one module must not report the shared env file stale \
             against just that module's own declarations: {env:?}"
        );
        assert!(
            !env.iter()
                .any(|e| e["kind"] == "env-var" && e["name"] == "EDITOR"),
            "the module's own EDITOR declaration matches the file and must \
             not be reported as drift either: {env:?}"
        );
    }

    /// The other half of `a_failed_env_check_is_never_reported_as_clean`,
    /// driven through the real command: a module cfgd cannot resolve is
    /// enough to fail the FULL-profile resolution the env check needs, and
    /// the Files and Packages diffs of the module actually asked about must
    /// survive it. Restoring the `?` on that resolution — or moving the call
    /// back inside the Shell section — turns `cfgd diff --module nvim` into an
    /// error that renders nothing, on any machine whose profile also declares
    /// a module with a broken dependency.
    #[test]
    #[serial]
    fn cmd_diff_module_keeps_its_own_diff_when_an_unrelated_module_cannot_resolve() {
        use crate::cli::helpers::tests::make_cli;

        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n",
        )
        .unwrap();
        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(
            profiles_dir.join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  modules:\n    - file-mod\n    - broken\n",
        )
        .unwrap();

        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let target = tmp_home.path().join("app.conf");

        let mod_dir = tmp.path().join("modules").join("file-mod");
        std::fs::create_dir_all(mod_dir.join("files")).unwrap();
        std::fs::write(mod_dir.join("files").join("app.conf"), "theme = dark\n").unwrap();
        std::fs::write(
            mod_dir.join("module.yaml"),
            format!(
                "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: file-mod\nspec:\n  files:\n    - source: files/app.conf\n      target: {}\n",
                cfgd_core::to_posix_string(&target)
            ),
        )
        .unwrap();

        let broken_dir = tmp.path().join("modules").join("broken");
        std::fs::create_dir_all(&broken_dir).unwrap();
        std::fs::write(
            broken_dir.join("module.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: broken\nspec:\n  depends: [nope]\n",
        )
        .unwrap();

        let mut cli = make_cli(config_path);
        cli.state_dir = Some(tmp.path().join("state"));
        cli.cache_dir = Some(tmp.path().join("cache"));
        let (printer, cap) = Printer::for_test_doc();

        cmd_diff(&cli, &printer, Some("file-mod"), false)
            .expect("one module's failure must not abort another module's diff");
        drop(printer);

        let json = cap.json().expect("diff emits a data payload");
        assert_eq!(
            json["summary"]["envCheckFailed"],
            serde_json::json!(true),
            "the env verdict is undetermined and must say so: {json}"
        );
        assert!(
            json["envCheckError"]
                .as_str()
                .is_some_and(|e| e.contains("nope") || e.contains("broken")),
            "the error must name what could not resolve: {json}"
        );
        let files = json["files"].as_array().expect("files array present");
        let target_posix = cfgd_core::to_posix_string(&target);
        assert!(
            files
                .iter()
                .any(|f| f["resourceId"] == target_posix.as_str() && f["matches"] == false),
            "the asked-for module's file diff must survive the other module's \
             failure: {json}"
        );
        // The `--module` path's combined env/alias section carries the same
        // name the machine path's does; the two are separate call sites and
        // only a positive assertion on each keeps them one word.
        let human = strip_ansi(&cap.human());
        assert!(
            human.contains("\nShell\n"),
            "the module path names its shell surface Shell too: {human}"
        );
    }

    fn strip_ansi(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' && chars.peek() == Some(&'[') {
                chars.next();
                for inner in chars.by_ref() {
                    if inner == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn print_package_drift_no_drift() {
        let (printer, cap) = Printer::for_test_doc();
        let mut payload = DiffOutput::default();
        let actions = vec![PackageAction::Skip {
            manager: "brew".into(),
            reason: "up to date".into(),
            origin: "profile".into(),
        }];
        {
            let section = printer.section_or_collapse("Packages");
            let has_drift = print_package_drift(
                &actions,
                &[],
                &section,
                &Owner::profile("tiny"),
                &mut payload,
            );
            assert!(!has_drift, "all-skip should report no drift");
        }
        drop(printer);

        let output = strip_ansi(&cap.human());
        assert!(
            !output.contains("Packages"),
            "a converged surface leaves no trace at all, got: {output}"
        );
        assert!(payload.packages.is_empty());
    }

    #[test]
    fn build_diff_doc_with_no_drift_emits_ok_no_drift() {
        let payload = DiffOutput {
            summary: DiffSummary {
                has_file_drift: false,
                has_pkg_drift: false,
                has_system_drift: false,
                system_check_failed: false,
                has_env_drift: false,
                env_check_failed: false,
            },
            ..Default::default()
        };
        let (printer, cap) = Printer::for_test_doc();
        printer.emit(build_diff_doc(&payload, DiffScope::Machine));
        drop(printer);
        let out = strip_ansi(&cap.human());
        assert!(
            out.contains("No drift detected"),
            "no-drift doc must say so: {out}"
        );
    }

    #[test]
    fn build_diff_doc_with_any_drift_emits_warn_drift_detected() {
        let payload = DiffOutput {
            summary: DiffSummary {
                has_file_drift: true,
                has_pkg_drift: false,
                has_system_drift: false,
                system_check_failed: false,
                has_env_drift: false,
                env_check_failed: false,
            },
            ..Default::default()
        };
        let (printer, cap) = Printer::for_test_doc();
        printer.emit(build_diff_doc(&payload, DiffScope::Machine));
        drop(printer);
        let out = strip_ansi(&cap.human());
        assert!(
            out.contains("Drift detected"),
            "drift doc must surface warning: {out}"
        );
    }

    /// A configurator whose check errored leaves the machine's state unknown.
    /// All three of the command's answers — the closing line, the `-o json`
    /// summary and the `--exit-code` status — must say so, because a script
    /// that reads any one of them as "clean" acts on a check that never ran.
    #[test]
    fn a_failed_system_check_is_never_reported_as_clean() {
        let payload = DiffOutput {
            system_errors: vec![SystemCheckError {
                key: "sysctl".to_string(),
                error: "permission denied".to_string(),
            }],
            summary: DiffSummary {
                has_file_drift: false,
                has_pkg_drift: false,
                has_system_drift: false,
                system_check_failed: true,
                has_env_drift: false,
                env_check_failed: false,
            },
            ..Default::default()
        };
        let (printer, cap) = Printer::for_test_doc();
        printer.emit(build_diff_doc(&payload, DiffScope::Machine));
        drop(printer);
        let doc_human = strip_ansi(&cap.human());
        assert!(
            !doc_human.contains("No drift detected"),
            "the summary must not report a verdict it does not have: {doc_human}"
        );
        assert!(
            doc_human.contains("Drift undetermined"),
            "the summary must name the gap: {doc_human}"
        );
        let json = cap.json().expect("diff emits a data payload");
        assert_eq!(
            json["summary"]["systemCheckFailed"],
            serde_json::json!(true)
        );
        assert_eq!(json["systemErrors"][0]["key"], serde_json::json!("sysctl"));

        assert_eq!(
            diff_exit_code(&payload.summary),
            Some(cfgd_core::exit::ExitCode::Error),
            "--exit-code must not report success on an unknown verdict"
        );

        // The same fact on the tally: a surface whose check could not run is
        // named by neither half of the closing line.
        let drifted = DiffOutput {
            files: vec![cfgd_core::providers::FileDriftResult {
                target: "/home/you/.zshrc".to_string(),
                matches: false,
                expected: "content matches source".to_string(),
                actual: "content differs from source".to_string(),
                unmanaged: false,
            }],
            summary: DiffSummary {
                has_file_drift: true,
                system_check_failed: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let tally = drift_tally(&drifted, DiffScope::Machine);
        assert!(
            !tally.contains("system"),
            "an unrun check is neither drifted nor clean: {tally}"
        );
        assert_eq!(tally, "1 file (packages, shell clean)");
    }

    /// The `diff --module` sibling of the test above: an unrelated module's
    /// full-profile resolution failure must read the same way a failed
    /// system-configurator check does, not as a clean env verdict.
    #[test]
    fn a_failed_env_check_is_never_reported_as_clean() {
        let payload = DiffOutput {
            env_check_error: Some("failed to fetch git source for module 'jarvis'".to_string()),
            summary: DiffSummary {
                has_file_drift: false,
                has_pkg_drift: false,
                has_system_drift: false,
                system_check_failed: false,
                has_env_drift: false,
                env_check_failed: true,
            },
            ..Default::default()
        };
        let (printer, cap) = Printer::for_test_doc();
        printer.emit(build_diff_doc(&payload, DiffScope::Machine));
        drop(printer);
        let doc_human = strip_ansi(&cap.human());
        assert!(
            !doc_human.contains("No drift detected"),
            "the summary must not report a verdict it does not have: {doc_human}"
        );
        assert!(
            doc_human.contains("Drift undetermined"),
            "the summary must name the gap: {doc_human}"
        );
        let json = cap.json().expect("diff emits a data payload");
        assert_eq!(json["summary"]["envCheckFailed"], serde_json::json!(true));
        assert_eq!(
            json["envCheckError"],
            serde_json::json!("failed to fetch git source for module 'jarvis'")
        );

        assert_eq!(
            diff_exit_code(&payload.summary),
            Some(cfgd_core::exit::ExitCode::Error),
            "--exit-code must not report success on an unknown verdict"
        );
    }

    /// Drift AND a check that could not run: the verdict names the drift, and
    /// the exit code is `Error` rather than `DriftDetected`. Without the
    /// reason on the line, a reader is left with "Drift detected" and an exit
    /// code that says something else went wrong.
    #[test]
    fn a_drifted_run_that_could_not_check_everything_still_names_the_gap() {
        let payload = DiffOutput {
            env_check_error: Some("failed to fetch git source for module 'jarvis'".to_string()),
            summary: DiffSummary {
                has_file_drift: true,
                has_pkg_drift: false,
                has_system_drift: false,
                system_check_failed: false,
                has_env_drift: false,
                env_check_failed: true,
            },
            ..Default::default()
        };
        let (printer, cap) = Printer::for_test_doc();
        printer.emit(build_diff_doc(&payload, DiffScope::Machine));
        drop(printer);
        let doc_human = strip_ansi(&cap.human());
        assert!(
            doc_human.contains("Drift detected"),
            "the drift it did find is still the verdict: {doc_human}"
        );
        assert!(
            doc_human.contains("the shell check could not run"),
            "the reason for exit 1 must be on the line that carries it: {doc_human}"
        );
        assert_eq!(
            diff_exit_code(&payload.summary),
            Some(cfgd_core::exit::ExitCode::Error),
            "an unrun check outranks ordinary drift in the exit code"
        );
    }

    #[test]
    fn a_clean_run_reports_its_clean_verdict_on_every_channel() {
        let (printer, cap) = Printer::for_test_doc();
        printer.emit(build_diff_doc(&DiffOutput::default(), DiffScope::Machine));
        drop(printer);
        let human = strip_ansi(&cap.human());
        assert!(
            human.contains("No drift detected"),
            "an all-checks-ran, no-drift run still says so: {human}"
        );
        assert!(
            !human.contains("clean)"),
            "a converged run names no surfaces — there is nothing to contrast \
             them against: {human}"
        );
        assert_eq!(
            diff_exit_code(&DiffSummary::default()),
            None,
            "nothing drifted and every check ran: exit 0"
        );
        assert_eq!(
            diff_exit_code(&DiffSummary {
                has_pkg_drift: true,
                ..Default::default()
            }),
            Some(cfgd_core::exit::ExitCode::DriftDetected),
            "ordinary drift keeps its own code"
        );
    }

    #[test]
    fn print_package_drift_skip_only_is_ignored_when_mixed_with_other_actions() {
        // Covers the `PackageAction::Skip` arm — a Skip mixed in with real
        // drift actions must not produce a payload entry of its own.
        let (printer, _cap) = Printer::for_test_doc();
        let mut payload = DiffOutput::default();
        let actions = vec![
            PackageAction::Skip {
                manager: "brew".into(),
                reason: "up to date".into(),
                origin: "profile".into(),
            },
            PackageAction::Install {
                manager: "cargo".into(),
                packages: vec!["ripgrep".into()],
                origin: "profile".into(),
            },
            PackageAction::Skip {
                manager: "npm".into(),
                reason: "managed externally".into(),
                origin: "profile".into(),
            },
        ];
        {
            let section = printer.section_or_collapse("Packages");
            let has_drift = print_package_drift(
                &actions,
                &[],
                &section,
                &Owner::profile("tiny"),
                &mut payload,
            );
            assert!(has_drift, "non-Skip actions count as drift");
        }
        drop(printer);
        // Two Skips + one Install — only the Install lands in payload.packages.
        assert_eq!(payload.packages.len(), 1);
        assert_eq!(payload.packages[0].manager, "cargo");
    }

    #[test]
    fn print_package_drift_missing_packages() {
        let (printer, cap) = Printer::for_test_doc();
        let mut payload = DiffOutput::default();
        let actions = vec![
            PackageAction::Install {
                manager: "cargo".into(),
                packages: vec!["ripgrep".into(), "fd-find".into()],
                origin: "profile".into(),
            },
            PackageAction::Uninstall {
                manager: "npm".into(),
                packages: vec!["left-pad".into()],
                origin: "profile".into(),
            },
        ];
        {
            let section = printer.section_or_collapse("Packages");
            let has_drift = print_package_drift(
                &actions,
                &[],
                &section,
                &Owner::profile("tiny"),
                &mut payload,
            );
            assert!(has_drift, "non-Skip actions should report drift");
        }
        drop(printer);

        let output = strip_ansi(&cap.human());
        assert!(
            output.contains("cargo: not installed") && output.contains("ripgrep"),
            "should show missing cargo packages, got: {output}"
        );
        assert!(
            output.contains("npm: extra") && output.contains("left-pad"),
            "should show extra npm packages, got: {output}"
        );
        assert!(
            output.contains("profile:tiny"),
            "package drift must group under its profile owner, got: {output}"
        );
        assert_eq!(payload.packages.len(), 2);
    }

    #[test]
    fn print_package_drift_reports_bootstrap_and_refusal() {
        // Ground truth for the wording: the deleted `PackageAction::Bootstrap`
        // arm (git show ef490085), carried into the `ManagerAction` vocabulary
        // that replaced it. `Refuse` is new — no manager surfaced a refusal
        // before this phase existed — so its line has no prior wording to
        // match and is asserted only for shape.
        let (printer, cap) = Printer::for_test_doc();
        let mut payload = DiffOutput::default();
        let pkg_actions = vec![PackageAction::Install {
            manager: "cargo".into(),
            packages: vec!["ripgrep".into()],
            origin: "profile".into(),
        }];
        let manager_actions = vec![
            ManagerAction::Provision {
                manager: "pipx".into(),
                via: "pip install pipx".into(),
                declared: None,
                batched: vec![],
                depends_on: vec![],
            },
            ManagerAction::Refuse {
                manager: "snap".into(),
                reason: "no available system manager".into(),
            },
        ];
        {
            let section = printer.section_or_collapse("Packages");
            let has_drift = print_package_drift(
                &pkg_actions,
                &manager_actions,
                &section,
                &Owner::profile("tiny"),
                &mut payload,
            );
            assert!(has_drift, "a bootstrap or a refusal is drift");
        }
        drop(printer);

        let output = strip_ansi(&cap.human());
        assert!(
            output.contains("pipx: not installed")
                && output.contains("can bootstrap via pip install pipx"),
            "should show the bootstrap need and its method, got: {output}"
        );
        assert!(
            output.contains("snap: not installed")
                && output.contains("cannot bootstrap: no available system manager"),
            "should show the refusal and its reason with a single separator \
             (the status renderer already supplies ' — ' before the detail), \
             got: {output}"
        );
        // A manager installs a manager, so it belongs to cfgd — same
        // attribution the plan that would provision it uses — and the
        // profile's own group (the declared package) still precedes it.
        let profile_at = output.find("profile:tiny").unwrap_or_else(|| {
            panic!("package drift must group under its profile owner, got: {output}")
        });
        let cfgd_at = output.find("cfgd:managers").unwrap_or_else(|| {
            panic!("a bootstrap/refusal must group under cfgd:managers, got: {output}")
        });
        assert!(
            profile_at < cfgd_at,
            "profile precedes cfgd in owner order, got: {output}"
        );

        assert_eq!(payload.packages.len(), 3);
        let bootstrap = payload
            .packages
            .iter()
            .find(|p| p.shape == "provision")
            .expect("a provision row must be in the json payload");
        assert_eq!(bootstrap.manager, "pipx");
        assert_eq!(
            bootstrap.bootstrap_method.as_deref(),
            Some("pip install pipx")
        );
        assert!(bootstrap.packages.is_empty());
        let refused = payload
            .packages
            .iter()
            .find(|p| p.shape == "refused")
            .expect("a refused row must be in the json payload");
        assert_eq!(refused.manager, "snap");
        assert_eq!(
            refused.reason.as_deref(),
            Some("no available system manager")
        );
        assert!(refused.packages.is_empty());
    }

    /// Minimal package-manager double: a fixed installed set plus an optional
    /// case-folding `package_identity`, to exercise `package_missing_drift`'s
    /// identity routing without shelling out to a real manager.
    struct FoldingStub {
        installed: std::collections::HashSet<String>,
        fold_case: bool,
    }

    impl cfgd_core::providers::PackageManager for FoldingStub {
        fn name(&self) -> &str {
            "chocolatey"
        }
        fn is_available(&self) -> bool {
            true
        }
        fn bootstrap_plan(&self) -> Option<cfgd_core::providers::BootstrapPlan> {
            None
        }
        fn bootstrap(
            &self,
            _cx: &cfgd_core::providers::PackageContext<'_>,
        ) -> cfgd_core::errors::Result<()> {
            Ok(())
        }
        fn installed_packages(
            &self,
            _cx: &cfgd_core::providers::PackageContext<'_>,
        ) -> cfgd_core::errors::Result<std::collections::HashSet<String>> {
            Ok(self.installed.clone())
        }
        fn install(
            &self,
            _packages: &[String],
            _cx: &cfgd_core::providers::PackageContext<'_>,
        ) -> cfgd_core::errors::Result<()> {
            Ok(())
        }
        fn uninstall(
            &self,
            _packages: &[String],
            _cx: &cfgd_core::providers::PackageContext<'_>,
        ) -> cfgd_core::errors::Result<()> {
            Ok(())
        }
        fn has_index(&self) -> bool {
            true
        }

        fn refresh_index(
            &self,
            _cx: &cfgd_core::providers::PackageContext<'_>,
        ) -> cfgd_core::errors::Result<()> {
            Ok(())
        }
        fn available_version(&self, _package: &str) -> cfgd_core::errors::Result<Option<String>> {
            Ok(None)
        }
        fn package_identity(&self, entry: &str) -> String {
            if self.fold_case {
                entry.to_ascii_lowercase()
            } else {
                entry.to_string()
            }
        }
    }

    fn resolved_pkg(manager: &str, resolved_name: &str) -> modules::ResolvedPackage {
        modules::ResolvedPackage {
            canonical_name: resolved_name.to_string(),
            resolved_name: resolved_name.to_string(),
            manager: manager.to_string(),
            manager_declared: false,
            version: None,
            script: None,
            creates: None,
            only_if: None,
            unless: None,
            min_version: None,
        }
    }

    // Both per-package drift walks — `cfgd diff` and `cfgd status` — call
    // `package_missing_drift` once per declared package. Without the memo each
    // call re-ran the manager's listing, which is the ~13s scan; with it the
    // manager answers once however many packages are checked.
    #[test]
    fn package_missing_drift_asks_a_manager_once_for_every_package_it_owns() {
        // The count is a memo-hit claim, so the memo's age ceiling is pinned out
        // of reach — unpinned it rests on the 30s wall clock. No serialization:
        // nothing in this crate's test binary pins the ceiling to zero, and a
        // longer ceiling can only let another test's entries live longer.
        let _ttl = cfgd_core::test_helpers::EnumerationMemoTtlGuard::never_expires();
        let enumerations = cfgd_core::test_helpers::measured_in_a_stable_generation(|| {
            let mgr = cfgd_core::test_helpers::MockPackageManager::new("npm")
                .with_installed(&["left-pad", "chalk"]);
            let enumerations = mgr.enumeration_counter();
            let mgr_map: std::collections::HashMap<
                String,
                &dyn cfgd_core::providers::PackageManager,
            > = [(
                "npm".to_string(),
                &mgr as &dyn cfgd_core::providers::PackageManager,
            )]
            .into_iter()
            .collect();

            let (printer, _cap) = Printer::for_test_doc();
            let state = cfgd_core::state::StateStore::open_in_memory().unwrap();
            let cx = cfgd_core::providers::PackageContext::new(&printer, &state);

            for name in ["left-pad", "chalk", "rimraf", "eslint", "prettier"] {
                package_missing_drift(&resolved_pkg("npm", name), &mgr_map, &cx);
            }

            enumerations.load(std::sync::atomic::Ordering::SeqCst)
        });

        assert_eq!(
            enumerations, 1,
            "five packages on one manager must cost one enumeration"
        );
    }

    #[test]
    fn package_missing_drift_routes_through_package_identity_for_case_insensitive_manager() {
        // The module desires `Wget` (as authored); chocolatey's installed set is
        // folded to `wget` (parse_choco_list lowercases). `package_missing_drift`
        // must match through package_identity — reverting the identity wire
        // re-reports the installed package as missing drift.
        let stub = FoldingStub {
            installed: ["wget".to_string()].into_iter().collect(),
            fold_case: true,
        };
        let mgr_map: std::collections::HashMap<String, &dyn cfgd_core::providers::PackageManager> =
            [(
                "chocolatey".to_string(),
                &stub as &dyn cfgd_core::providers::PackageManager,
            )]
            .into_iter()
            .collect();

        let (printer, _cap) = Printer::for_test_doc();
        let state = cfgd_core::state::StateStore::open_in_memory().unwrap();
        let cx = cfgd_core::providers::PackageContext::new(&printer, &state);
        let pkg = resolved_pkg("chocolatey", "Wget");
        assert!(
            package_missing_drift(&pkg, &mgr_map, &cx).is_none(),
            "desired `Wget` must match folded installed `wget` — no drift"
        );
    }

    #[test]
    fn package_missing_drift_reports_genuinely_absent_package() {
        let stub = FoldingStub {
            installed: std::collections::HashSet::new(),
            fold_case: true,
        };
        let mgr_map: std::collections::HashMap<String, &dyn cfgd_core::providers::PackageManager> =
            [(
                "chocolatey".to_string(),
                &stub as &dyn cfgd_core::providers::PackageManager,
            )]
            .into_iter()
            .collect();

        let (printer, _cap) = Printer::for_test_doc();
        let state = cfgd_core::state::StateStore::open_in_memory().unwrap();
        let cx = cfgd_core::providers::PackageContext::new(&printer, &state);
        let pkg = resolved_pkg("chocolatey", "wget");
        let drift = package_missing_drift(&pkg, &mgr_map, &cx).expect("absent package must drift");
        assert_eq!(drift.shape, "missing");
        assert_eq!(drift.packages, vec!["wget".to_string()]);
    }

    #[test]
    fn package_missing_drift_skips_script_packages() {
        let mgr_map: std::collections::HashMap<String, &dyn cfgd_core::providers::PackageManager> =
            std::collections::HashMap::new();
        let (printer, _cap) = Printer::for_test_doc();
        let state = cfgd_core::state::StateStore::open_in_memory().unwrap();
        let cx = cfgd_core::providers::PackageContext::new(&printer, &state);
        let pkg = resolved_pkg("script", "rustup");
        assert!(package_missing_drift(&pkg, &mgr_map, &cx).is_none());
    }
}
