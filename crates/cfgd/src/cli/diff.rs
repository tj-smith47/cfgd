use super::*;

use cfgd_core::PathDisplayExt;
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

/// Keep only the records worth reporting: a converged file is the absence of
/// a finding, and listing every one of them would bury the drifted and the
/// unevaluable entries a consumer actually acts on. Classifies the record
/// first (a target cfgd never wrote is a different finding whose fix is a
/// decision rather than a re-write), pushes it into the payload, and answers
/// whether it drifted. That answer is all the machine-wide path needs — its
/// findings were already minted by the engine — while the scoped path reads
/// the drifted record back off the payload's tail to word a finding with the
/// record's own post-classification literals, so the store and the payload
/// cannot disagree.
fn record_file_drift(
    payload: &mut DiffOutput,
    mut record: cfgd_core::providers::FileDriftResult,
    strategy: cfgd_core::config::FileStrategy,
    config_dir: &std::path::Path,
    state: &cfgd_core::state::StateStore,
) -> bool {
    if record.matches {
        return false;
    }
    cfgd_core::reconciler::mark_unmanaged_drift(&mut record, strategy, config_dir, state);
    payload.files.push(record);
    true
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
        return cmd_diff_module(&ctx, mod_name, exit_code);
    }

    printer.heading("Diff");

    let (cfg, profile_name, local_resolved) = ctx.config_and_profile()?;
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
    let mut registry = desired.take_registry(cfg);
    let composed_sources = desired.sources;
    let mut resolved = desired.resolved;
    let resolved_modules = desired.modules;

    // Emitted after the resolve, not before it: the header names what the
    // profile RESOLVES to, and a `depends` pulls a module the declared list
    // never mentions into the set the findings below are reported against.
    printer.kv_rows(cfgd_core::output::config_header_rows(
        &cfgd_core::output::ConfigHeader {
            config_path: Some(&cli.config),
            sources: &composed_sources,
            profile: Some(profile_name),
            profile_inherits: &resolved.inherits_chain(),
            modules: &cfgd_core::output::HeaderModule::of_resolved(&resolved_modules),
        },
    ));

    ctx.resolve_manifest_packages(&mut resolved.merged.packages)?;
    // The engine probes system configurators, some of which resolve
    // config-relative paths; `status`/`verify`/`plan`/`apply` all hand the
    // config dir over, and the one scan `diff` consumes must see the same
    // configurator set they do.
    registry.set_system_config_dir(config_dir);

    let mut diff_payload = DiffOutput::default();

    let state = ctx.state()?;
    let cfgd_installed = cfgd_installed_packages(state)?;
    let fm = CfgdFileManager::new(config_dir, &resolved)?;
    // ONE walk: the shared live-drift engine finds and records every drift row
    // (and the checks that could not run), exactly as `status --scan` and
    // `verify` do — so no surface can disagree about what drifted. Everything
    // below is presentation over its report; the inline hunks are re-rendered
    // only for the entries the engine already found drifted.
    let report = {
        let pkg_cx = cfgd_core::providers::PackageContext::new(printer, state);
        super::live_drift::live_drift_results(
            config_dir,
            &resolved,
            &registry,
            &resolved_modules,
            &cfgd_installed,
            state,
            &pkg_cx,
            &fm,
        )?
    };
    // The engine recorded its findings; the scan stamp is the caller's, the
    // same split `status --scan` observes.
    state.record_scan();

    // Every check that could not run, whichever pass reported it: the payload
    // and the exit gate read one list, and only the RENDER files them by
    // section.
    let all_check_errors = report.all_check_errors();
    let drifted_ids: std::collections::HashSet<&str> = report
        .findings
        .iter()
        .filter(|r| matches!(r.resource_type.as_str(), "file" | "module"))
        .map(|r| r.resource_id.as_str())
        .collect();

    let has_file_drift = {
        let files_phase = printer.section_or_collapse("Files");
        // The file renderers take a bare `&Printer` and know nothing of the
        // tree; depth inheritance is what lands their per-file lines inside
        // the owner group opened around them.
        let _inherit = printer.depth_inheritance();
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
        let mut drift = false;
        {
            let _group = files_phase.section_owner_or_collapse(&profile_owner.label());
            // Target order, as `fm.diff` sorted: two runs finding the same
            // drift read the same, whatever the declaration order was.
            for managed in crate::files::CfgdFileManager::sorted_managed_specs(&resolved.merged) {
                let rid = cfgd_core::expand_tilde(&managed.target).display_posix();
                if !drifted_ids.contains(rid.as_str()) {
                    continue;
                }
                let record = fm.diff_managed_one(managed, printer)?;
                let strategy = strategies.for_target(&cfgd_core::expand_tilde(&managed.target));
                if record_file_drift(&mut diff_payload, record, strategy, config_dir, state) {
                    drift = true;
                }
            }
        }
        // Module-deployed files render the same inline content diff as profile
        // files (module sources carry no tera origin, so pass None).
        for module in modules_by_name(&resolved_modules) {
            let _group =
                files_phase.section_owner_or_collapse(&OwnerLabel::new("module", &module.name));
            for file in files_by_target(module) {
                let rid = super::live_drift::module_file_spec_resource_id(&module.name, file);
                if !drifted_ids.contains(rid.as_str()) {
                    continue;
                }
                let strategy = strategies
                    .for_target(&cfgd_core::expand_tilde(std::path::Path::new(&file.target)));
                let record =
                    diff_module_file(&fm, &resolved, module, file, config_dir, strategy, printer)?;
                if record_file_drift(&mut diff_payload, record, strategy, config_dir, state) {
                    drift = true;
                }
            }
        }
        drift
    };

    let has_pkg_drift = {
        let pkg_sec = printer.section_or_collapse("Packages");
        // A version finding is a package row: the plan below diffs NAMES, and
        // the floor half of the same walk answers for the copies the machine
        // holds. Sorted by id so one machine reads the same however the
        // effective walk reached it.
        let mut version_rows: Vec<&cfgd_core::reconciler::VerifyResult> = report
            .findings
            .iter()
            .filter(|r| {
                r.resource_type == "package" && !super::live_drift::is_presence_package_row(r)
            })
            .collect();
        version_rows.sort_by(|a, b| a.resource_id.cmp(&b.resource_id));
        // The report carries the scan's own package plan, so this section
        // prices exactly what the recorder wrote instead of planning again.
        print_package_drift(
            &report.pkg_actions,
            &report.manager_actions,
            &version_rows,
            &report.package_check_errors,
            &pkg_sec,
            &profile_owner,
            &mut diff_payload,
        )
    };

    let has_env_drift = {
        let env_sec = printer.section_or_collapse("Shell");
        let mut drift = false;
        {
            let env_group = env_sec.section_owner_or_collapse(&profile_owner.label());
            let results = env_drift_ordered(
                report
                    .findings
                    .iter()
                    .filter(|r| cfgd_core::output::is_shell_drift_kind(&r.resource_type))
                    .cloned()
                    .collect(),
            );
            let drop_env_file_row = cfgd_core::output::env_file_row_is_redundant(
                results.iter().map(|r| r.resource_type.as_str()),
            );
            let merged_env_items = cfgd_core::reconciler::MergedEnvItems::new(
                &resolved.merged.env,
                &resolved.merged.aliases,
                &resolved.merged.entry_owners,
                &resolved_modules,
                &report.path_dirs,
            );
            for r in results {
                drift = true;
                // An env-var/alias row's `expected`/`actual` are opaque markers —
                // neither real value ever flows into a persisted or gateway-shipped
                // drift record — so recompute both here, for this terminal/`-o json`
                // display only. The recorder already stored the markers.
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

    let has_system_drift = {
        let sys_sec = printer.section_or_collapse("System");
        // Every system key resolves against the merged profile ⊕ module view,
        // which is what puts a system action under the profile owner in the
        // plan too (`owner_of`'s fall-through arm).
        let sys_group = sys_sec.section_owner_or_collapse(&profile_owner.label());
        // The engine answered in registration order; this surface reads by
        // key, so sort at the render (and in the payload) only. A drift row's
        // id is `<configurator>.<key>` and an error row's is the bare
        // configurator name, so one string sort interleaves the two the way
        // the old per-configurator walk did — which holds because `.` (0x2E)
        // sorts below every character of a registered configurator name (all
        // alphanumeric), keeping one name's `<name>.<key>` rows contiguous
        // ahead of the next name; a configurator named with a character
        // below `.` (`-`, `+`) would silently reorder this section.
        let mut sys_rows: Vec<&cfgd_core::reconciler::VerifyResult> = report
            .findings
            .iter()
            .filter(|r| r.resource_type == "system")
            .collect();
        sys_rows.sort_by(|a, b| a.resource_id.cmp(&b.resource_id));
        // System rows only: a package's unanswerable floor rendered under this
        // heading would file it beside the configurator probes it has nothing
        // to do with. The PAYLOAD still carries every check error (below), so
        // the exit gate and every structured consumer see one list.
        let mut check_errors = report.check_errors.clone();
        check_errors.sort_by(|a, b| a.key.cmp(&b.key));

        let mut errors = check_errors.iter().peekable();
        for row in &sys_rows {
            while let Some(err) = errors.peek() {
                if err.key.as_str() > row.resource_id.as_str() {
                    break;
                }
                sys_group
                    .status(Role::Warn, err.key.clone())
                    .qualifier("error checking drift")
                    .detail(&err.error);
                errors.next();
            }
            // A configurator that found no setting at all hands back an empty
            // `actual`; the fold states the absence instead of rendering
            // `have: `.
            let (expected, actual) =
                cfgd_core::output::drift_operands("system", &row.expected, &row.actual);
            sys_group
                .status(Role::Warn, row.resource_id.clone())
                .drift(&expected, &actual);
            diff_payload.system.push(SystemDriftOutput {
                key: row.resource_id.clone(),
                expected: row.expected.clone(),
                actual: row.actual.clone(),
            });
        }
        for err in errors {
            sys_group
                .status(Role::Warn, err.key.clone())
                .qualifier("error checking drift")
                .detail(&err.error);
        }
        diff_payload.system_errors = all_check_errors;
        !sys_rows.is_empty()
    };

    // Rows the walk above could not re-find (a bare legacy module id, a
    // system key outside every configurator this scan evaluated, a check
    // error's own key) — the store's own answer, kept unresolved by
    // `live_drift_results` and rendered here through the same drift-row
    // renderer every section above used, never a second wording.
    let has_standing_drift = {
        let sec = printer.section_or_collapse("Standing");
        let _inherit = printer.depth_inheritance();
        for e in &report.standing {
            let (expected, actual) = cfgd_core::output::drift_operands(
                &e.resource_type,
                e.expected.as_deref().unwrap_or_default(),
                e.actual.as_deref().unwrap_or_default(),
            );
            sec.status(
                Role::Warn,
                cfgd_core::output::drift_item_subject(&e.resource_type, &e.resource_id),
            )
            .drift(&expected, &actual);
        }
        !report.standing.is_empty()
    };
    diff_payload.standing = report.standing;

    diff_payload.summary = DiffSummary {
        has_file_drift,
        has_pkg_drift,
        has_system_drift,
        system_check_failed: !diff_payload.system_errors.is_empty(),
        has_env_drift,
        // The full desired state this path needs for every phase was already
        // resolved above (before any section opened); a failure there aborts
        // the whole command via `?`, so this path never reaches here with the
        // env check unresolved.
        env_check_failed: false,
        has_standing_drift,
    };

    printer.emit(build_diff_doc(&diff_payload, DiffScope::Machine));

    if exit_code && let Some(code) = diff_exit_code(&diff_payload.summary) {
        code.exit();
    }

    Ok(())
}

/// Env findings in the order they render: by kind, then by the item named —
/// aliases ahead of env vars, as every surface naming the shell pair renders
/// them.
///
/// `env_verify_results` answers in check order (the managed file and its rc
/// lines, then each declared item), which says nothing about the machine — two
/// runs finding the same drift must read the same rather than reordering by
/// whatever the check reached first.
pub(super) fn env_drift_ordered(
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
    let any_drift = summary.has_file_drift
        || summary.has_pkg_drift
        || summary.has_system_drift
        || summary.has_env_drift
        || summary.has_standing_drift;
    let check_failed = summary.system_check_failed || summary.env_check_failed;
    if !cfgd_core::reconciler::has_any_drift(any_drift, check_failed) {
        return None;
    }
    if check_failed {
        return Some(cfgd_core::exit::ExitCode::Error);
    }
    Some(cfgd_core::exit::ExitCode::DriftDetected)
}

/// The scoped run's header, the same rows `apply --module` opens on: a title
/// that owns no rows would put its blank line straight under the heading,
/// which no other titled run does.
///
/// The isolate resolves no profile, so it carries no `Profile` row, and its
/// `Modules` row is delta-only like every other isolate surface: the heading
/// already names the module the invocation named, so the row renders only what
/// the resolution ADDED. The config it read still declares its subscriptions,
/// and those are a fact about where this run's configuration comes from
/// whether or not a profile resolved.
fn emit_isolate_header(
    ctx: &RunContext<'_>,
    modules: &[cfgd_core::output::HeaderModule],
) -> anyhow::Result<()> {
    let declared =
        cfgd_core::reconciler::ComposedSource::from_declared(&ctx.config()?.spec.sources);
    ctx.printer().kv_rows(cfgd_core::output::config_header_rows(
        &cfgd_core::output::ConfigHeader {
            config_path: Some(&ctx.cli().config),
            sources: &declared,
            profile: None,
            profile_inherits: &[],
            modules,
        },
    ));
    Ok(())
}

fn cmd_diff_module(ctx: &RunContext<'_>, mod_name: &str, exit_code: bool) -> anyhow::Result<()> {
    let cli = ctx.cli();
    let printer = ctx.printer();
    let config_dir = ctx.config_dir();
    let registry = ctx.base_registry();
    let platform = Platform::current();
    let mgr_map = registry.manager_map();
    let cache_base = module_cache_dir(cli)?;
    let pkg_cx = ctx.package_context()?;
    let resolved_modules = match modules::resolve_modules(
        &[mod_name.to_string()],
        config_dir,
        &cache_base,
        &[],
        platform,
        &mgr_map,
        Some(&pkg_cx),
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
            emit_isolate_header(ctx, &[])?;
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
        Err(e) => {
            emit_isolate_header(ctx, &[])?;
            return Err(e.into());
        }
    };
    emit_isolate_header(
        ctx,
        &cfgd_core::output::HeaderModule::of_isolate(&resolved_modules),
    )?;

    let state = ctx.state()?;
    let pkg_cx = cfgd_core::providers::PackageContext::new(printer, state);

    let mut diff_payload = DiffOutput::default();
    // The scoped record's two halves (module doc in `live_drift`): every key
    // this run re-checked, and the non-matching subset in its producer's own
    // literals. The Shell section below joins BOTH for the entries this
    // module's chain owns; the whole env file and the rc lines are the whole
    // profile's artifacts and stay outside this run's module scope.
    let mut checked: Vec<(String, String)> = Vec::new();
    let mut findings: Vec<cfgd_core::reconciler::VerifyResult> = Vec::new();
    let mut has_file_diff = false;
    let mut has_pkg_drift = false;
    let resolved = empty_resolved_profile(&[mod_name.to_string()], &ctx.active_profile_name());

    {
        // Mirror the full `cmd_diff` path: the `Files` heading, one
        // `module:<name>` group per module, the shared per-file inline-diff
        // renderer. Module sources carry no tera origin (None).
        let files_phase = printer.section_or_collapse("Files");
        let _inherit = printer.depth_inheritance();
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
                let rid = super::live_drift::module_file_resource_id(&module.name, &record.target);
                checked.push(("module".to_string(), rid.clone()));
                if record_file_drift(&mut diff_payload, record, strategy, config_dir, state)
                    && let Some(rec) = diff_payload.files.last()
                {
                    findings.push(cfgd_core::reconciler::VerifyResult {
                        resource_type: "module".to_string(),
                        resource_id: rid,
                        matches: false,
                        expected: rec.expected.clone(),
                        actual: rec.actual.clone(),
                        unmanaged: rec.unmanaged,
                    });
                    has_file_diff = true;
                }
            }
        }
    }

    // The declared-floor pass over this chain's own packages, through the ONE
    // engine the full walk reads: a scoped surface resolves the version rows it
    // re-checks, so it evaluates what it resolves.
    let (version_rows, package_check_errors) =
        super::live_drift::scoped_version_drift(&resolved, &resolved_modules, registry, &pkg_cx)?;
    {
        let pkg_sec = printer.section_or_collapse("Packages");
        for module in modules_by_name(&resolved_modules) {
            let group = pkg_sec.section_owner_or_collapse(&OwnerLabel::new("module", &module.name));
            let mut packages: Vec<&modules::ResolvedPackage> = module.packages.iter().collect();
            packages.sort_by(|a, b| a.resolved_name.cmp(&b.resolved_name));
            for pkg in packages {
                // A `script` package and an unregistered manager are questions
                // nothing can answer, so neither joins the scope this run can
                // vouch for.
                if pkg.manager != "script" && mgr_map.contains_key(pkg.manager.as_str()) {
                    checked.push((
                        "package".to_string(),
                        package_entry_drift_id(
                            &pkg.manager,
                            &pkg.resolved_name,
                            mgr_map.get(pkg.manager.as_str()).copied(),
                        ),
                    ));
                }
                if let Some(drift) = package_missing_drift(pkg, &mgr_map, &pkg_cx) {
                    has_pkg_drift = true;
                    findings.push(cfgd_core::reconciler::VerifyResult {
                        resource_type: "package".to_string(),
                        resource_id: package_entry_drift_id(
                            &pkg.manager,
                            &pkg.resolved_name,
                            mgr_map.get(pkg.manager.as_str()).copied(),
                        ),
                        matches: false,
                        expected: cfgd_core::PACKAGE_WANT_INSTALLED.to_string(),
                        // The RECORDED operand takes the one stored spelling
                        // for a missing package (`Absence::NotInstalled`);
                        // `drift.shape` stays the `-o json` payload's own
                        // wire value untouched.
                        actual: cfgd_core::Absence::NotInstalled.as_str().to_string(),
                        unmanaged: false,
                    });
                    group
                        .status(Role::Warn, pkg.manager.clone())
                        .qualifier(cfgd_core::Absence::NotInstalled.as_str())
                        .detail(pkg.resolved_name.clone());
                    diff_payload.packages.push(drift);
                    continue;
                }
                let id = package_entry_drift_id(
                    &pkg.manager,
                    &pkg.resolved_name,
                    mgr_map.get(pkg.manager.as_str()).copied(),
                );
                if let Some(row) = version_rows.iter().find(|r| r.resource_id == id) {
                    has_pkg_drift = true;
                    findings.push(row.clone());
                    group
                        .status(Role::Warn, row.resource_id.clone())
                        .drift(&row.expected, &row.actual);
                    diff_payload.packages.push(version_package_drift(row));
                } else if let Some(err) = package_check_errors.iter().find(|e| e.key == id) {
                    group
                        .status(Role::Warn, err.key.clone())
                        .qualifier("error checking drift")
                        .detail(&err.error);
                }
            }
        }
    }

    let (has_env_drift, env_check_failed) = {
        let env_sec = printer.section_or_collapse("Shell");
        // Only the per-item check, over the isolate's own merge: each entry a
        // module of this chain declares, judged against the line the primary
        // managed env file holds. The whole-file staleness row, the rc source
        // lines and the folded `PATH` line stay the machine-wide walk's — the
        // whole profile's shared artifacts, which one module's fragment can
        // neither vouch for nor blame.
        let check = cfgd_core::reconciler::env_item_verify_results(
            &resolved.merged.env,
            &resolved.merged.aliases,
            &resolved.merged.entry_owners,
            &resolved_modules,
        );
        // The ONE ownership answer, asked once for the whole block: which
        // layer's declaration each checked name belongs to — the same fold
        // the isolate's merge just applied.
        let owners = cfgd_core::reconciler::merged_entry_owners(&resolved, &resolved_modules);
        let chain_tokens: std::collections::HashSet<String> = resolved_modules
            .iter()
            .map(|m| cfgd_core::reconciler::Owner::module(&m.name).token())
            .collect();
        // The scoped check's env scope is the names the module's CHAIN owns,
        // judged by `merged_entry_owners` — records, resolves, renders and
        // exits all in that one scope, so a scoped diff can clear the very
        // rows a scoped diff records instead of stranding them for a
        // machine-wide walk. The ids are machine-scope (`env-var:EDITOR`
        // names the deployed line), so an entry whose recorded winner is a
        // layer OUTSIDE this chain stays out of `checked`: a scoped "clean"
        // may not heal a claim it never re-checked. Whole-file staleness, rc
        // source lines and the folded `PATH` line stay the machine-wide
        // walk's.
        let chain_owned = |r: &cfgd_core::reconciler::VerifyResult| {
            let owner = if r.resource_type == "alias" {
                owners.aliases.get(&r.resource_id)
            } else {
                owners.env.get(&r.resource_id)
            };
            owner.is_some_and(|o| chain_tokens.contains(o.as_str()))
        };
        let (owned, _foreign): (Vec<_>, Vec<_>) =
            check.results.into_iter().partition(|r| chain_owned(r));
        for r in &owned {
            checked.push((r.resource_type.clone(), r.resource_id.clone()));
        }
        let results = env_drift_ordered(owned);
        {
            // `path_dirs` feeds only the folded `PATH` line, which the scoped
            // check never renders a row for.
            let merged_env_items = cfgd_core::reconciler::MergedEnvItems::new(
                &resolved.merged.env,
                &resolved.merged.aliases,
                &resolved.merged.entry_owners,
                &resolved_modules,
                &[],
            );
            // One pass: every finding lands in exactly one bucket — the
            // fold's recorded winner when that token names a chain module,
            // else the module under report (the only module the caller asked
            // about, exactly as the scoped status does for an unsplittable
            // file id) — so no finding can fall between the groups.
            let tokens: std::collections::HashMap<String, &str> = resolved_modules
                .iter()
                .map(|m| {
                    (
                        cfgd_core::reconciler::Owner::module(&m.name).token(),
                        m.name.as_str(),
                    )
                })
                .collect();
            let mut grouped: std::collections::HashMap<
                &str,
                Vec<&cfgd_core::reconciler::VerifyResult>,
            > = std::collections::HashMap::new();
            for r in &results {
                let owner = if r.resource_type == "alias" {
                    owners.aliases.get(&r.resource_id)
                } else {
                    owners.env.get(&r.resource_id)
                };
                let module_name = owner
                    .and_then(|o| tokens.get(o.as_str()).copied())
                    .unwrap_or(mod_name);
                grouped.entry(module_name).or_default().push(r);
            }
            for module in modules_by_name(&resolved_modules) {
                let Some(mine) = grouped.remove(module.name.as_str()) else {
                    continue;
                };
                let group =
                    env_sec.section_owner_or_collapse(&OwnerLabel::new("module", &module.name));
                for r in mine {
                    // The stored operands are opaque markers; recompute the
                    // real declared/deployed lines for this display only,
                    // exactly as the machine-wide Shell section does.
                    let (expected, actual) = merged_env_items
                        .display_values(&r.resource_type, &r.resource_id)
                        .unwrap_or_else(|| (r.expected.clone(), r.actual.clone()));
                    let (expected, actual) =
                        cfgd_core::output::drift_operands(&r.resource_type, &expected, &actual);
                    group
                        .status(
                            Role::Warn,
                            cfgd_core::output::drift_item_subject(&r.resource_type, &r.resource_id),
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
        }
        findings.extend(results);
        if let Some(err) = &check.check_error {
            // The same first-class row an erroring system check renders, so a
            // probe that could not run is never read as clean; the path folds
            // to `~/` like every display slot, the payload keeps it absolute.
            env_sec
                .status(Role::Warn, cfgd_core::fold_home_in_text(&err.key))
                .qualifier("error checking drift")
                .detail(&err.error);
            diff_payload.env_check_error = Some(err.error.clone());
        }
        (!diff_payload.env.is_empty(), check.check_error.is_some())
    };

    // The scoped record: this run's own module rows, and nothing beyond
    // them — no machine-wide stamp, unlike the full-machine path above.
    // Returns the rows this chain owns that the scan above did not cover
    // (a bare legacy module id): rendered and priced beside the findings
    // above, through the same drift-row renderer.
    let standing = super::live_drift::record_scoped_scan_findings(
        state,
        &checked,
        &findings,
        &package_check_errors,
        &resolved_modules,
        registry,
    );
    let has_standing_drift = {
        let sec = printer.section_or_collapse("Standing");
        let _inherit = printer.depth_inheritance();
        for e in &standing {
            let (expected, actual) = cfgd_core::output::drift_operands(
                &e.resource_type,
                e.expected.as_deref().unwrap_or_default(),
                e.actual.as_deref().unwrap_or_default(),
            );
            sec.status(
                Role::Warn,
                cfgd_core::output::drift_item_subject(&e.resource_type, &e.resource_id),
            )
            .drift(&expected, &actual);
        }
        !standing.is_empty()
    };
    diff_payload.standing = standing;

    let package_check_failed = !package_check_errors.is_empty();
    diff_payload.system_errors = package_check_errors;
    diff_payload.summary = DiffSummary {
        has_file_drift: has_file_diff,
        has_pkg_drift,
        // A single module's diff evaluates no system configurator, so the
        // system verdict is neither drifted nor undetermined here.
        has_system_drift: false,
        // …but a floor it could not read is still a check that did not run,
        // and the exit gate ranks that above the drift it did find.
        system_check_failed: package_check_failed,
        has_env_drift,
        env_check_failed,
        has_standing_drift,
    };

    printer.emit(build_diff_doc(&diff_payload, DiffScope::Module(mod_name)));

    if exit_code && let Some(code) = diff_exit_code(&diff_payload.summary) {
        code.exit();
    }

    Ok(())
}

/// The ONE grammar for a package drift's `resource_id`:
/// `<manager>:<package_identity>`, one package per row.
///
/// Every CLI minting site reaches it through here — `package_action_drift`
/// (`live_drift.rs`) rows per package, and `cmd_diff_module` /
/// `cmd_status_module` for both their `checked` scope and their
/// missing-package findings — so a whole-machine check, a `--module` scan and
/// the daemon tick spell the same package identically and each heals what the
/// others recorded. The composer itself is core's, because the tick's and the
/// apply's producers live there.
pub(super) use cfgd_core::reconciler::package_entry_drift_id;

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
        expected: None,
        actual: None,
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
/// `version_rows` are the declared-floor half of the same walk (a package the
/// machine holds whose installed copy is below its `minVersion`), and
/// `check_errors` the floors it could not answer for at all. Both name a
/// PACKAGE, so both read under the package owner rather than beside the
/// system probes.
pub(super) fn print_package_drift(
    pkg_actions: &[PackageAction],
    manager_actions: &[ManagerAction],
    version_rows: &[&cfgd_core::reconciler::VerifyResult],
    check_errors: &[SystemCheckError],
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
    let has_drift =
        !pkg_diffs.is_empty() || !manager_actions.is_empty() || !version_rows.is_empty();
    if !has_drift && check_errors.is_empty() {
        return false;
    }
    let managers_owner = Owner::cfgd(MANAGERS_GROUP);
    let mut owners: Vec<Owner> = Vec::new();
    if !pkg_diffs.is_empty() || !version_rows.is_empty() || !check_errors.is_empty() {
        owners.push(profile.clone());
    }
    if !manager_actions.is_empty() {
        owners.push(managers_owner.clone());
    }
    Owner::order(&mut owners);
    for owner in &owners {
        let group = section.section_owner(&owner.label());
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
                                expected: None,
                                actual: None,
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
                            expected: None,
                            actual: None,
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
                        expected: None,
                        actual: None,
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
                        expected: None,
                        actual: None,
                    });
                }
                PackageAction::Skip { .. } => {}
            }
        }
        for row in version_rows {
            group
                .status(Role::Warn, row.resource_id.clone())
                .drift(&row.expected, &row.actual);
            payload.packages.push(version_package_drift(row));
        }
        // A floor nothing could answer for is rendered exactly as every other
        // unanswerable check is, under the package it names. Its payload row
        // stays in `systemErrors` with the rest, which is what the exit gate
        // and every structured consumer read.
        for err in check_errors {
            group
                .status(Role::Warn, err.key.clone())
                .qualifier("error checking drift")
                .detail(&err.error);
        }
    }
    has_drift
}

/// The `-o json` entry a version-drift row serializes as: the `outdated` shape,
/// with the declared floor and the version the machine holds in `expected` /
/// `actual`. The ONE composition, read by the full walk and by `--module`, so a
/// consumer cannot meet two spellings of one finding; the manager and package
/// halves come back from the id's own splitter.
pub(super) fn version_package_drift(row: &cfgd_core::reconciler::VerifyResult) -> PackageDrift {
    let (manager, packages) =
        cfgd_core::reconciler::split_package_drift_resource_id(&row.resource_id)
            .unwrap_or((row.resource_id.as_str(), Vec::new()));
    PackageDrift {
        manager: manager.to_string(),
        shape: "outdated".to_string(),
        packages: packages.iter().map(|p| (*p).to_string()).collect(),
        bootstrap_method: None,
        reason: None,
        expected: Some(row.expected.clone()),
        actual: Some(row.actual.clone()),
    }
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
        // Like the system surface below, a scoped diff evaluates only the
        // entries its own modules declare — never the whole shell surface —
        // so its verdict may not call that surface clean.
        (
            "shell",
            "shell item",
            shell_rows,
            s.has_env_drift,
            scope == DiffScope::Machine && !s.env_check_failed,
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
    // Standing rows are not one of the checked surfaces above — they are
    // rows a check outside this run's own scope already found and this run
    // is only carrying forward — so they add a clause without a "clean"
    // counterpart.
    if !output.standing.is_empty() {
        drifted.push(format!(
            "{} standing",
            cfgd_core::pluralize(output.standing.len(), "row")
        ));
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
        || output.summary.has_env_drift
        || output.summary.has_standing_drift;
    // A run that could not check everything has no clean verdict to give, so
    // it never renders one — whether or not the checks that DID run found
    // drift.
    let check_failed = output.summary.system_check_failed || output.summary.env_check_failed;
    let role = if cfgd_core::reconciler::has_any_drift(any_drift, check_failed) {
        Role::Warn
    } else {
        Role::Ok
    };
    let mut reasons = Vec::new();
    if output.summary.system_check_failed {
        // Not "a system check": the same slot now carries a package's
        // unanswerable version floor, and the verdict may not name a category
        // the failed check did not come from.
        reasons.push("a drift check could not run".to_string());
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
        // the hand-edited line is derived from `MergedEnvItems::declared_line` —
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
        let declared_alias = cfgd_core::config::ShellAlias {
            name: "ll".to_string(),
            command: "ls -la".to_string(),
            platforms: vec![],
        };
        let declared_owners = {
            let mut o = cfgd_core::config::EntryOwners::default();
            o.claim(
                "profile:default",
                &[],
                std::slice::from_ref(&declared_alias),
            );
            o
        };
        let declared_line = cfgd_core::reconciler::MergedEnvItems::new(
            &[],
            std::slice::from_ref(&declared_alias),
            &declared_owners,
            &[],
            &[],
        )
        .declared_line("alias", "ll")
        .expect("alias renders a declared line");
        std::fs::write(
            cfgd_core::reconciler::primary_env_file(tmp_home.path()),
            format!("# managed by cfgd \u{2014} do not edit\n{declared_line}\n"),
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
    /// declaring a `scoop` package plus a state store recording `scoop`'s
    /// bootstrap PATH dir reproduces exactly that machine: managed env files
    /// written byte-for-byte with the recorded PATH line must report no `env`
    /// drift.
    ///
    /// `recorded_manager_path_dirs` filters recorded dirs by the manager NAMES
    /// `effective_desired_packages` holds, with no availability or platform
    /// test, so the subject is live on every host — and `scoop` is a Windows
    /// bootstrap manager, which makes Windows the platform it matters most on.
    /// The manager is chosen for being INSTALLED nowhere cfgd's CI runs (no
    /// manager is `cfg`-gated off; `is_available` is `command_available` on the
    /// tool's own name), so package planning shells out to nothing. That is a
    /// hermeticity property of the fixture, never a reason to gate the test:
    /// even a host that has scoop would answer the env question the same way.
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
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  envScope: Interactive\n  packages:\n    scoop:\n      - black\n",
        )
        .unwrap();

        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        // Byte-for-byte what the engine writes from the recorded bootstrap
        // dir, taken from the engine itself: a PATH line owed entirely to
        // bootstrap dirs has no declaration behind it, so `declared_line`
        // cannot answer for it, and hand-spelling one file would miss the
        // second file Windows also writes when Git Bash is present.
        let path_dirs = vec![cfgd_core::reconciler::ManagerPathDir::new(
            "scoop",
            "/opt/scoop/bin",
        )];
        let written: Vec<String> = cfgd_core::reconciler::MergedEnvItems::new(
            &[],
            &[],
            &Default::default(),
            &[],
            &path_dirs,
        )
        .managed_env_files(tmp_home.path(), cfgd_core::config::EnvScope::Interactive)
        .into_iter()
        .map(|(path, content)| {
            cfgd_core::ensure_parent_dir(&path).unwrap();
            std::fs::write(&path, content).unwrap();
            cfgd_core::to_posix_string(&path)
        })
        .collect();
        assert!(
            !written.is_empty(),
            "a recorded bootstrap dir alone must produce a managed env file"
        );

        let state_dir = tmp.path().join("state");
        let mut cli = make_cli(config_path);
        cli.state_dir = Some(state_dir.clone());
        cli.cache_dir = Some(tmp.path().join("cache"));

        // The same state a real `cfgd verify` (or an apply that bootstrapped
        // scoop) would have recorded.
        let state = open_state_store(Some(&state_dir), cfgd_core::Scope::User).unwrap();
        state
            .record_bootstrapped_path_dirs("scoop", &["/opt/scoop/bin".to_string()])
            .unwrap();
        drop(state);

        let (printer, cap) = Printer::for_test_doc();
        cmd_diff(&cli, &printer, None, false).unwrap();
        drop(printer);

        let json = cap.json().expect("diff emits a data payload");
        let env = json["env"].as_array().expect("env array present");
        // Every file the engine wrote, not just the primary one: Windows adds
        // the bash file when Git Bash is present, and a stale second file is
        // the same bug reported under another name.
        assert!(
            !env.iter().any(|e| e["kind"] == "env"
                && e["name"]
                    .as_str()
                    .is_some_and(|n| written.iter().any(|w| w == n))),
            "no managed env file may report stale once diff passes the same \
             recorded bootstrap PATH dirs verify does (wrote {written:?}): {env:?}"
        );
    }

    /// Both scoped verbs stay blind to the MACHINE-WIDE env surface: the
    /// managed env file's own staleness row, the rc source lines, and every
    /// entry a layer outside the module's chain owns (the profile's PAGER
    /// here). The file is the whole profile's shared artifact — one module's
    /// fragment can neither vouch for nor blame it — so neither verb renders
    /// or records a row for it. What `diff --module` DOES evaluate is the
    /// per-item check over the entries the module itself owns (EDITOR),
    /// which is a finding about the module, not about the shared file —
    /// `a_module_diff_reports_only_the_envs_that_module_owns` pins that
    /// half. Re-adding the whole-file or rc-line comparison to a scoped verb
    /// is exactly what this pins against.
    #[test]
    #[serial]
    fn both_scoped_verbs_stay_blind_to_a_machine_wide_env_drift() {
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
        std::fs::create_dir_all(&mod_dir).unwrap();
        std::fs::write(
            mod_dir.join("module.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: env-mod\nspec:\n  env:\n    - name: EDITOR\n      value: vim\n",
        )
        .unwrap();

        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        // A hand-edited managed env file: the whole-machine check would call
        // this stale (PAGER rewritten, EDITOR missing). The body is
        // deliberately a line no generator ever wrote, so no dialect is
        // right — but the PATH is, or the file does not exist at all on a
        // platform whose primary env file carries another name.
        std::fs::write(
            cfgd_core::reconciler::primary_env_file(tmp_home.path()),
            "# managed by cfgd \u{2014} do not edit\nexport PAGER=\"more\"\n",
        )
        .unwrap();

        let mut cli = make_cli(config_path);
        let state_dir = tmp.path().join("state");
        cli.state_dir = Some(state_dir.clone());
        cli.cache_dir = Some(tmp.path().join("cache"));

        let (printer, cap) = Printer::for_test_doc();
        cmd_diff(&cli, &printer, Some("env-mod"), false).unwrap();
        drop(printer);

        let json = cap.json().expect("diff emits a data payload");
        let env = json["env"].as_array().expect("env array present");
        assert!(
            !env.iter()
                .any(|e| e["kind"] == "env" || e["kind"] == "env-rc"),
            "a scoped diff never evaluates the shared file or rc lines: {env:?}"
        );
        assert!(
            !env.iter().any(|e| e["name"] == "PAGER"),
            "a profile-owned entry never joins a module's findings: {env:?}"
        );
        assert_eq!(json["summary"]["envCheckFailed"], serde_json::json!(false));

        let (printer, buf) = Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
        super::super::verify::cmd_verify(&cli, &printer, Some("env-mod"), false).unwrap();
        drop(printer);
        let out = cfgd_core::test_helpers::captured_text(&buf);
        let env_file_name = cfgd_core::reconciler::primary_env_file(tmp_home.path())
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .expect("the primary env file has a name");
        assert!(
            !out.contains(&env_file_name) && !out.contains("PAGER"),
            "a scoped verify may not render the machine-wide env comparison: {out}"
        );

        // The RECORD half of the agreement: neither verb minted a row for
        // drift its scope cannot vouch for — the shared file, the rc lines,
        // the profile-owned entry. The module-owned EDITOR row `diff` DOES
        // record is pinned by its own test.
        let store =
            crate::cli::registry::open_state_store(Some(&state_dir), cfgd_core::Scope::User)
                .unwrap();
        let rows = store.unresolved_drift().unwrap();
        assert!(
            !rows
                .iter()
                .any(|r| matches!(r.resource_type.as_str(), "env" | "env-rc")
                    || r.resource_id == "PAGER"),
            "no scoped verb records a row outside its module's ownership: {rows:?}"
        );
    }

    /// A scoped diff evaluates exactly the env/alias entries the module's own
    /// chain declares — `merged_entry_owners` over the isolate is the one
    /// ownership answer — and nothing the profile (or any other layer) owns:
    /// a profile-owned entry drifting on the machine leaves `diff --module
    /// <other>` clean, while the module that DOES own a missing entry reports
    /// it, renders it under its own `module:<name>` group, and records it so
    /// the store's row heals when a later check re-finds the line.
    #[test]
    #[serial]
    fn a_module_diff_reports_only_the_envs_that_module_owns() {
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
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  envScope: Interactive\n  env:\n    - name: PAGER\n      value: less\n  modules:\n    - env-mod\n    - other-mod\n",
        )
        .unwrap();
        let env_mod_dir = tmp.path().join("modules").join("env-mod");
        std::fs::create_dir_all(&env_mod_dir).unwrap();
        std::fs::write(
            env_mod_dir.join("module.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: env-mod\nspec:\n  depends:\n    - aaa-dep\n  env:\n    - name: EDITOR\n      value: vim\n",
        )
        .unwrap();
        // A dependency that ALSO declares EDITOR (outvoted: the fold visits
        // it first, so env-mod's value wins) and declares VISUAL alone — an
        // entry whose owner CANNOT be the reported module, so an ownership
        // lookup that defaulted to the reported module cannot pass by luck.
        let dep_mod_dir = tmp.path().join("modules").join("aaa-dep");
        std::fs::create_dir_all(&dep_mod_dir).unwrap();
        std::fs::write(
            dep_mod_dir.join("module.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: aaa-dep\nspec:\n  env:\n    - name: EDITOR\n      value: emacs\n    - name: VISUAL\n      value: micro\n",
        )
        .unwrap();
        let other_mod_dir = tmp.path().join("modules").join("other-mod");
        std::fs::create_dir_all(&other_mod_dir).unwrap();
        std::fs::write(
            other_mod_dir.join("module.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: other-mod\nspec: {}\n",
        )
        .unwrap();

        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        // The profile-owned PAGER line is hand-edited (drifted) and the
        // module-owned EDITOR line is absent. The hand-edited body is a line
        // no generator wrote, so it needs no dialect — the path does.
        std::fs::write(
            cfgd_core::reconciler::primary_env_file(tmp_home.path()),
            "# managed by cfgd \u{2014} do not edit\nexport PAGER=\"more\"\n",
        )
        .unwrap();

        let mut cli = make_cli(config_path);
        let state_dir = tmp.path().join("state");
        cli.state_dir = Some(state_dir.clone());
        cli.cache_dir = Some(tmp.path().join("cache"));

        // The module that owns nothing on the drifted surface stays clean:
        // PAGER is the profile's, and a scoped diff may not blame or report it.
        let (printer, cap) = Printer::for_test_doc();
        cmd_diff(&cli, &printer, Some("other-mod"), false).unwrap();
        drop(printer);
        let json = cap.json().expect("diff emits a data payload");
        assert_eq!(
            json["env"].as_array().map(Vec::as_slice),
            Some(&[][..]),
            "a module owning no env entry reports none: {json}"
        );
        assert_eq!(json["summary"]["hasEnvDrift"], serde_json::json!(false));

        // The module that DOES own a missing entry reports exactly it.
        let (printer, cap) = Printer::for_test_doc();
        cmd_diff(&cli, &printer, Some("env-mod"), false).unwrap();
        drop(printer);
        let json = cap.json().expect("diff emits a data payload");
        let env = json["env"].as_array().expect("env array present");
        assert!(
            env.iter()
                .any(|e| e["kind"] == "env-var" && e["name"] == "EDITOR"),
            "the module-owned missing entry is the finding: {env:?}"
        );
        assert!(
            !env.iter().any(|e| e["name"] == "PAGER"),
            "the profile-owned entry never joins a module's findings: {env:?}"
        );
        assert_eq!(
            env.iter().filter(|e| e["name"] == "EDITOR").count(),
            1,
            "two declarers, one merged entry, one finding: {env:?}"
        );
        assert_eq!(json["summary"]["hasEnvDrift"], serde_json::json!(true));
        let human = strip_ansi(&cap.human());
        assert!(
            human.contains("env: EDITOR"),
            "the finding renders as a Shell row: {human}"
        );
        assert!(
            human.contains("module:env-mod"),
            "the finding renders under its owner's group: {human}"
        );
        // The dependency-owned entry is the structural proof: VISUAL's owner
        // CANNOT be the reported module (env-mod never declares it), so an
        // ownership lookup that defaulted to the reported module fails here
        // rather than passing by luck. Groups render in module-name order —
        // aaa-dep's group with its own entry ahead of env-mod's with the
        // contested one.
        let dep_group = human.find("module:aaa-dep").expect("dep group opens");
        let visual = human.find("env: VISUAL").expect("dep-owned finding");
        let mod_group = human.find("module:env-mod").unwrap();
        let editor = human.find("env: EDITOR").unwrap();
        assert!(
            dep_group < visual && visual < mod_group && mod_group < editor,
            "each finding renders under the module the merge says owns it, \
             never blanket-attributed to the reported module: {human}"
        );
        assert!(
            human.contains("vim") && !human.contains("emacs"),
            "the contested entry's declared operand is the merge winner's value: {human}"
        );

        // The record half: the scoped check recorded the row it can vouch
        // for, under the id grammar every env producer mints, and nothing
        // for the surfaces (whole file, rc lines) outside its scope.
        let store =
            crate::cli::registry::open_state_store(Some(&state_dir), cfgd_core::Scope::User)
                .unwrap();
        let rows = store.unresolved_drift().unwrap();
        assert!(
            rows.iter()
                .any(|r| r.resource_type == "env-var" && r.resource_id == "EDITOR"),
            "the module-owned finding is recorded: {rows:?}"
        );
        assert!(
            !rows
                .iter()
                .any(|r| matches!(r.resource_type.as_str(), "env" | "env-rc")
                    || r.resource_id == "PAGER"),
            "nothing outside the module's ownership is recorded: {rows:?}"
        );
    }

    /// The HEAL half of the scoped env contract: what a scoped diff records
    /// a scoped diff can clear. The per-item check evaluates exactly the
    /// entries the module's chain owns (`merged_entry_owners` over the
    /// isolate), so those keys join the `checked` scope and a recorded
    /// env-var/alias row heals the moment a scoped re-check finds the line
    /// back in place — a module-only workflow no longer leaves rows only a
    /// machine-wide walk could resolve.
    #[test]
    #[serial]
    fn a_scoped_diff_clears_the_env_row_it_recorded_once_the_machine_converges() {
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
        let env_mod_dir = tmp.path().join("modules").join("env-mod");
        std::fs::create_dir_all(&env_mod_dir).unwrap();
        std::fs::write(
            env_mod_dir.join("module.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: env-mod\nspec:\n  env:\n    - name: EDITOR\n      value: vim\n",
        )
        .unwrap();

        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        // The machine has CONVERGED: the module-owned line is back in place.
        // Both the file's name and the line's dialect are the running
        // platform's, so they come from production's own renderers rather
        // than a bash-shaped literal.
        let declared_env = vec![cfgd_core::config::EnvVar {
            name: "EDITOR".to_string(),
            value: "vim".to_string(),
            platforms: vec![],
        }];
        let declared_owners = {
            let mut o = cfgd_core::config::EntryOwners::default();
            o.claim("module:env-mod", &declared_env, &[]);
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
        std::fs::write(
            cfgd_core::reconciler::primary_env_file(tmp_home.path()),
            format!("# managed by cfgd \u{2014} do not edit\n{declared_line}\n"),
        )
        .unwrap();

        let mut cli = make_cli(config_path);
        let state_dir = tmp.path().join("state");
        cli.state_dir = Some(state_dir.clone());
        cli.cache_dir = Some(tmp.path().join("cache"));

        {
            // What an earlier scoped check recorded while the line was gone —
            // the module-owned row, and a profile-owned one beside it that a
            // scoped heal may NOT touch (its name is outside this module's
            // ownership, so this run cannot vouch for it either way).
            let store =
                crate::cli::registry::open_state_store(Some(&state_dir), cfgd_core::Scope::User)
                    .unwrap();
            store
                .record_drift(
                    "env-var",
                    "EDITOR",
                    Some("current"),
                    Some("missing or changed"),
                    "local",
                )
                .unwrap();
            store
                .record_drift(
                    "env-var",
                    "PAGER",
                    Some("current"),
                    Some("missing or changed"),
                    "local",
                )
                .unwrap();
        }

        let printer = cfgd_core::test_helpers::test_printer();
        cmd_diff(&cli, &printer, Some("env-mod"), false).unwrap();

        let store =
            crate::cli::registry::open_state_store(Some(&state_dir), cfgd_core::Scope::User)
                .unwrap();
        let rows = store.unresolved_drift().unwrap();
        assert!(
            !rows
                .iter()
                .any(|r| r.resource_type == "env-var" && r.resource_id == "EDITOR"),
            "a scoped re-check that finds the module-owned line in place \
             heals the row a scoped check recorded: {rows:?}"
        );
        assert!(
            rows.iter()
                .any(|r| r.resource_type == "env-var" && r.resource_id == "PAGER"),
            "a profile-owned row stays outside the scoped heal's scope: {rows:?}"
        );
    }

    /// A scoped diff whose env probe itself fails — the managed env file
    /// exists but cannot be read — reports the failure as a first-class row
    /// and flags `envCheckFailed`, the state `diff_exit_code` maps to
    /// `Error` (1) ahead of `DriftDetected` (5): unknown outranks known on
    /// the scoped surface exactly as on the three unscoped ones
    /// (`tests/drift_exit_code.rs`). The fixture puts a DIRECTORY at the
    /// primary env file's path, so the read fails with something other than
    /// NotFound even under root — and on every OS, which unix mode bits
    /// cannot do (`fs_perms` no-ops them on Windows).
    #[test]
    #[serial]
    fn a_module_diff_reports_an_env_probe_it_could_not_run() {
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
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  envScope: Interactive\n  modules:\n    - env-mod\n",
        )
        .unwrap();
        let env_mod_dir = tmp.path().join("modules").join("env-mod");
        std::fs::create_dir_all(&env_mod_dir).unwrap();
        std::fs::write(
            env_mod_dir.join("module.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: env-mod\nspec:\n  env:\n    - name: EDITOR\n      value: vim\n",
        )
        .unwrap();

        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        std::fs::create_dir_all(cfgd_core::reconciler::primary_env_file(tmp_home.path())).unwrap();

        let mut cli = make_cli(config_path);
        cli.state_dir = Some(tmp.path().join("state"));
        cli.cache_dir = Some(tmp.path().join("cache"));

        let (printer, cap) = Printer::for_test_doc();
        cmd_diff(&cli, &printer, Some("env-mod"), false).unwrap();
        drop(printer);
        let json = cap.json().expect("diff emits a data payload");
        assert_eq!(
            json["summary"]["envCheckFailed"],
            serde_json::json!(true),
            "an unreadable env file is a failed check, not a clean surface: {json}"
        );
        assert!(
            json["envCheckError"]
                .as_str()
                .is_some_and(|e| !e.is_empty()),
            "the payload names why the probe failed: {json}"
        );
        assert_eq!(
            json["env"].as_array().map(Vec::as_slice),
            Some(&[][..]),
            "no per-item verdict survives a probe that never answered: {json}"
        );
        let human = strip_ansi(&cap.human());
        assert!(
            human.contains("error checking drift"),
            "the failed probe renders as its own row: {human}"
        );

        // The exit mapping over exactly the summary this run put on the wire.
        let summary = DiffSummary {
            has_file_drift: json["summary"]["hasFileDrift"] == serde_json::json!(true),
            has_pkg_drift: json["summary"]["hasPkgDrift"] == serde_json::json!(true),
            has_system_drift: false,
            system_check_failed: false,
            has_env_drift: json["summary"]["hasEnvDrift"] == serde_json::json!(true),
            env_check_failed: true,
            has_standing_drift: json["summary"]["hasStandingDrift"] == serde_json::json!(true),
        };
        assert_eq!(
            diff_exit_code(&summary),
            Some(cfgd_core::exit::ExitCode::Error),
            "the scoped surface exits Error for a check that could not run"
        );
    }

    /// A scoped diff resolves only its own module's chain, so a SIBLING
    /// module cfgd cannot resolve is invisible to it: the Files diff of the
    /// module actually asked about renders, and the env verdict stays the
    /// scoped path's unconditional not-evaluated settle rather than an
    /// undetermined failure borrowed from a resolution this run never needed.
    /// Re-adding a full-profile resolution to `cmd_diff_module` — for the env
    /// half or anything else — is what would flip these.
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
            serde_json::json!(false),
            "the scoped path evaluates no env surface, so nothing failed: {json}"
        );
        assert_eq!(
            json["envCheckError"],
            serde_json::Value::Null,
            "a resolution this run never needed cannot leave an error: {json}"
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
        // The scoped path opens no Shell section at all: the env surface is
        // the whole profile's, outside one module's scope.
        let human = strip_ansi(&cap.human());
        assert!(
            !human.contains("\nShell\n"),
            "a scoped diff renders no machine-wide shell surface: {human}"
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
                &[],
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
                has_standing_drift: false,
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
                has_standing_drift: false,
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
                has_standing_drift: false,
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
                has_standing_drift: false,
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
                has_standing_drift: false,
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
                &[],
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

    /// The wire contract of a version finding: the `outdated` shape carries the
    /// declared floor and the installed version, and the row a reader sees
    /// states the same pair. A PRESENCE row serializes neither key, so a
    /// consumer can tell the two findings apart by shape alone.
    #[test]
    fn a_version_row_renders_and_serializes_its_two_operands() {
        let (printer, cap) = Printer::for_test_doc();
        let mut payload = DiffOutput::default();
        let (floor, installed) = ("2".to_string(), "1.0.0".to_string());
        let row = cfgd_core::reconciler::VerifyResult {
            resource_type: "package".to_string(),
            resource_id: "dnf:demo".to_string(),
            matches: false,
            expected: floor,
            actual: installed,
            unmanaged: false,
        };
        let missing = vec![PackageAction::Install {
            manager: "cargo".into(),
            packages: vec!["ripgrep".into()],
            origin: "profile".into(),
        }];
        {
            let section = printer.section_or_collapse("Packages");
            let has_drift = print_package_drift(
                &missing,
                &[],
                &[&row],
                &[],
                &section,
                &Owner::profile("tiny"),
                &mut payload,
            );
            assert!(has_drift, "a version row is drift");
        }
        drop(printer);

        let output = strip_ansi(&cap.human());
        assert!(
            output.contains("dnf:demo")
                && output.contains("want: 2")
                && output.contains("have: 1.0.0"),
            "the row states both operands, got: {output}"
        );

        let json = serde_json::to_value(&payload).expect("payload serializes");
        let entries = json["packages"].as_array().expect("packages array");
        let outdated = entries
            .iter()
            .find(|e| e["shape"] == "outdated")
            .unwrap_or_else(|| panic!("the version finding carries the outdated shape: {json}"));
        assert_eq!(outdated["manager"], "dnf");
        assert_eq!(outdated["packages"], serde_json::json!(["demo"]));
        assert_eq!(outdated["expected"], "2");
        assert_eq!(outdated["actual"], "1.0.0");

        let presence = entries
            .iter()
            .find(|e| e["shape"] == "missing")
            .unwrap_or_else(|| panic!("the presence finding keeps its own shape: {json}"));
        assert!(
            presence.get("expected").is_none() && presence.get("actual").is_none(),
            "a presence finding serializes no version operands: {presence}"
        );
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
                &[],
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
                &[],
                &[],
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
        fn bootstrap_plan_given(
            &self,
            _delivered: &dyn Fn(&str) -> bool,
        ) -> Option<cfgd_core::providers::BootstrapPlan> {
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
