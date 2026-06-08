use super::*;

use cfgd_core::PathDisplayExt;
use cfgd_core::output::{Doc, Role};

/// Terminal outcome of an apply run: the final status plus, when a signal
/// cooperatively aborted the apply, the conventional exit code (`130` SIGINT /
/// `143` SIGTERM) so `cmd_apply` can exit with it.
#[derive(Debug)]
pub struct ApplyOutcome {
    pub status: cfgd_core::state::ApplyStatus,
    pub aborted_code: Option<u8>,
}

impl ApplyOutcome {
    /// A non-action terminal path (dry-run, nothing-to-do, declined-confirm):
    /// success, no abort.
    fn success() -> Self {
        Self {
            status: cfgd_core::state::ApplyStatus::Success,
            aborted_code: None,
        }
    }
}

pub fn cmd_apply(
    cli: &Cli,
    printer: &cfgd_core::output::Printer,
    args: &ApplyArgs,
) -> anyhow::Result<()> {
    let outcome = run_apply(cli, printer, args)?;

    // A graceful signal abort is NOT an error, but it must NOT exit 0: exit with
    // the signal-conventional code so wrappers see the interruption. The abort
    // message + structured payload are already flushed by `run_apply`.
    if let Some(code) = outcome.aborted_code {
        std::process::exit(code as i32);
    }

    // A partial or total apply failure must surface as a nonzero exit so CI `&&`
    // chains and the daemon don't treat a broken apply as success. The structured
    // payload is already flushed by `run_apply`; exit directly (mirrors
    // status/diff/upgrade) so render_cli_error doesn't double-print a failure line.
    if matches!(
        outcome.status,
        cfgd_core::state::ApplyStatus::Partial | cfgd_core::state::ApplyStatus::Failed
    ) {
        cfgd_core::exit::ExitCode::ApplyFailed.exit();
    }

    Ok(())
}

/// Drive a full apply (or dry-run) and return the resulting [`ApplyOutcome`]
/// so the caller can map a partial/total failure to a nonzero process exit and
/// a signal abort to its conventional exit code.
///
/// Non-apply terminal paths (dry-run, aborted-confirmation, nothing-to-do)
/// report [`ApplyStatus::Success`] with no abort code — they did not run
/// actions, so they never warrant a failure exit. Keeping the exit decision in
/// `cmd_apply` lets in-process tests capture the rendered failure shape without
/// `process::exit` aborting the harness.
pub fn run_apply(
    cli: &Cli,
    printer: &cfgd_core::output::Printer,
    args: &ApplyArgs,
) -> anyhow::Result<ApplyOutcome> {
    // Parse --context (mirrors PlanArgs::context).
    let reconcile_context = match args.context.as_str() {
        "apply" => ReconcileContext::Apply,
        "reconcile" => ReconcileContext::Reconcile,
        other => {
            anyhow::bail!(
                "Unknown context '{}'. Valid values: apply, reconcile",
                other
            );
        }
    };

    // --from: clone from git source or use local path as config directory.
    // When --config points to a non-default path, use its parent as the clone target
    // so the cloned config ends up where the user expects.
    if let Some(from) = &args.from {
        let cli_config_dir = cli.config.parent().map(|p| p.to_path_buf());
        let default_dir = cfgd_core::default_config_dir();
        let target = if let Some(ref dir) = cli_config_dir {
            if *dir != default_dir && !cli.config.exists() {
                Some(dir.as_path())
            } else {
                None
            }
        } else {
            None
        };
        init::resolve_from(from, target, "master", printer)?;
    }

    let dry_run = args.dry_run;
    let yes = args.yes;
    let skip = &args.skip;
    let only = &args.only;
    let module_filter = args.module.as_deref();
    if dry_run {
        printer.heading("Plan");
    } else {
        printer.heading("Apply");
    }

    let config_dir = config_dir(cli);

    // When --module is set, try loading profile but fall back to empty if none configured
    let (cfg, resolved) = if let Some(mod_name) = module_filter {
        match load_config_and_profile(cli) {
            Ok((cfg, profile_name, resolved)) => {
                printer.kv_block([
                    ("Config".to_string(), cli.config.display_posix()),
                    ("Profile".to_string(), profile_name),
                ]);
                (cfg, resolved)
            }
            Err(e) => {
                tracing::debug!("profile load failed, using module-only mode: {}", e);
                let cfg =
                    config::load_config(&cli.config).unwrap_or_else(|_| config::minimal_config());
                let resolved = empty_resolved_profile(mod_name);
                printer.kv_block([
                    ("Config".to_string(), cli.config.display_posix()),
                    ("Profile".to_string(), "(module-only)".to_string()),
                ]);
                (cfg, resolved)
            }
        }
    } else {
        let (cfg, profile_name, resolved) = load_config_and_profile(cli)?;
        printer.kv_block([
            ("Config".to_string(), cli.config.display_posix()),
            ("Profile".to_string(), profile_name),
        ]);
        (cfg, resolved)
    };

    // Open state only after config discovery so a missing config (or an
    // unresolvable home) surfaces before any state.db is created — otherwise a
    // NoConfig exit would leave an orphan state directory behind.
    let state = open_state_store(cli.state_dir.as_deref())?;

    let mut registry = build_registry_with_config(Some(&cfg));
    registry.set_system_config_dir(&config_dir);

    // `ApplyPhase` (clap ValueEnum) is already validated at parse time.
    let phase_filter: Option<PhaseName> = args.phase.map(apply_phase_to_phase_name);

    // Compose with sources if configured
    let (source_env, source_commits) = if !cfg.spec.sources.is_empty() {
        let composition_result = compose_with_sources(cli, &cfg, &resolved, printer)?;
        let se = composition_result.source_env;
        let sc = composition_result.source_commits;
        ((Some(composition_result.resolved), se), sc)
    } else {
        (
            (None, std::collections::HashMap::new()),
            std::collections::HashMap::new(),
        )
    };
    let mut effective_resolved = source_env.0.unwrap_or(resolved);
    let source_env = source_env.1;

    // Resolve manifest files (Brewfile, package.json, etc.) into package lists
    packages::resolve_manifest_packages(&mut effective_resolved.merged.packages, &config_dir)?;

    // Extend registry with custom package managers from config
    registry.package_managers.extend(packages::custom_managers(
        &effective_resolved.merged.packages.custom,
    ));

    // Resolve modules
    let module_names = if let Some(mod_name) = module_filter {
        vec![mod_name.to_string()]
    } else {
        effective_resolved.merged.modules.clone()
    };

    let resolved_modules = if !module_names.is_empty() {
        let platform = Platform::detect();
        let mgr_map = managers_map(&registry);
        let cache_base = modules::default_module_cache_dir()?;
        match modules::resolve_modules(
            &module_names,
            &config_dir,
            &cache_base,
            &platform,
            &mgr_map,
            printer,
        ) {
            Ok(mods) => mods,
            Err(e) if module_filter.is_some() => {
                tracing::debug!("module filter '{}' not found: {}", module_names[0], e);
                Vec::new()
            }
            Err(e) => return Err(e.into()),
        }
    } else {
        Vec::new()
    };

    // If --module is set, skip profile-level packages/files
    let module_only = module_filter.is_some();

    // Declarative prune (and the post-apply tracking-table GC below) reconcile
    // removals, which is only safe on a FULL, unscoped run: it needs the
    // complete desired set to know a package has truly left the config. A scoped
    // apply (--phase / --only / --skip / --skip-scripts) or a module-only run
    // sees a partial picture, so prune/GC are suppressed there — a
    // not-applied-this-run consumer might still need the package.
    let scope_restricted =
        phase_filter.is_some() || !skip.is_empty() || !only.is_empty() || args.skip_scripts;
    let prune_eligible = !module_only && !scope_restricted;

    // In dry-run mode we don't need secret providers wired up — just plan files for display.
    // In apply mode we wire up the full file manager with secret providers.
    let (pkg_actions, file_actions, dry_run_fm) = if module_only {
        (Vec::new(), Vec::new(), None)
    } else {
        let all_managers: Vec<&dyn cfgd_core::providers::PackageManager> = registry
            .package_managers
            .iter()
            .map(|m| m.as_ref())
            .collect();
        let cfgd_installed = if prune_eligible {
            cfgd_installed_packages(&state)?
        } else {
            std::collections::HashSet::new()
        };
        // Profile-scoped: module packages are added separately by
        // `reconciler.plan` as `Action::Module`, so this planner must stay
        // profile-only to avoid double-handling them.
        let pkg = packages::plan_packages(
            &effective_resolved.merged,
            &[],
            &all_managers,
            &cfgd_installed,
        )?;

        let mut fm = CfgdFileManager::new(&config_dir, &effective_resolved)?;
        fm.set_global_strategy(cfg.spec.file_strategy);
        if !source_env.is_empty() {
            fm.set_source_env(&source_env);
        }

        if !dry_run {
            let (backend_name, age_key_path) = secret_backend_from_config(Some(&cfg));
            fm.set_secret_providers(
                Some(secrets::build_secret_backend(
                    &backend_name,
                    age_key_path,
                    Some(&config_dir),
                )),
                secrets::build_secret_providers(),
            );
        }

        let fa = fm.plan(&effective_resolved.merged)?;

        if dry_run {
            // Keep fm around for diff display but don't register it
            (pkg, fa, Some(fm))
        } else {
            // Register the file manager so the reconciler delegates through the trait
            registry.file_manager = Some(Box::new(fm));
            (pkg, fa, None)
        }
    };

    let reconciler = Reconciler::new(&registry, &state);
    let mut plan = reconciler.plan(
        &effective_resolved,
        file_actions,
        pkg_actions,
        resolved_modules.clone(),
        reconcile_context,
    )?;

    // Snapshot scope before --skip/--only prune the plan, so a zero-action
    // outcome distinguishes "in sync" from "a filter excluded pending work".
    let filter_active =
        phase_filter.is_some() || !skip.is_empty() || !only.is_empty() || args.skip_scripts;
    let module_miss = module_filter
        .filter(|_| resolved_modules.is_empty())
        .map(str::to_string);
    let scope = ScopeReport::capture(&plan, filter_active, module_miss);

    // Apply --skip / --only filters
    filter_plan(&mut plan, skip, only);

    // Strip script phases when --skip-scripts is set
    if args.skip_scripts {
        strip_scripts_from_plan(&mut plan);
    }

    if dry_run {
        display_plan_preview(
            &plan,
            printer,
            &state,
            &args.context,
            phase_filter.as_ref(),
            dry_run_fm.as_ref(),
            &scope,
        );
        // Preview orphaned custom-manager packages a real apply would prune
        // (read-only — execute nothing here). Same gating + query as the apply
        // path so the preview matches the action.
        if prune_eligible {
            preview_orphaned_custom_packages(&state, &registry, printer);
        }
        return Ok(ApplyOutcome::success());
    }

    // --- Apply mode ---

    // Handle unmanaged file targets: if a target exists as a non-cfgd file, prompt to
    // adopt (proceed), backup (rename to .cfgd-backup), or skip.
    handle_unmanaged_file_targets(&mut plan, &config_dir, &state, printer, yes)?;

    // Self-heal the package-tracking table on a full unscoped apply, BEFORE the
    // no-op early-return: a row whose package vanished (partial-uninstall
    // failure or out-of-band removal) produces no plan action, so it would never
    // be reached after the `has_actions` gate. Best-effort.
    if prune_eligible {
        let all_managers: Vec<&dyn cfgd_core::providers::PackageManager> = registry
            .package_managers
            .iter()
            .map(|m| m.as_ref())
            .collect();
        gc_stale_package_tracking(&state, &all_managers);
        gc_orphaned_custom_packages(&state, &registry, printer);
    }

    // Check if filtered plan has actions
    let has_actions = if let Some(ref pf) = phase_filter {
        plan.phases.iter().any(|p| {
            p.actions
                .iter()
                .any(|a| reconciler::action_matches_phase_filter(&p.name, a, pf))
        })
    } else {
        !plan.is_empty()
    };

    if !has_actions {
        report_no_in_scope_actions(printer, &scope, phase_filter.as_ref());
        printer.emit(Doc::new().with_data(ApplyOutput::nothing_to_do()));
        return Ok(ApplyOutcome::success());
    }

    let start = std::time::Instant::now();

    // Show what will change, nested under a section so each phase's items
    // render at the section's indent. The preview honours the same
    // action-level filter as the executor so users see exactly what's about
    // to run.
    {
        let preview = printer.section("Plan preview");
        for phase_item in &plan.phases {
            let items = reconciler::format_plan_items(phase_item);
            let displayed: Vec<&String> = if let Some(ref pf) = phase_filter {
                phase_item
                    .actions
                    .iter()
                    .zip(items.iter())
                    .filter_map(|(a, item)| {
                        reconciler::action_matches_phase_filter(&phase_item.name, a, pf)
                            .then_some(item)
                    })
                    .collect()
            } else {
                items.iter().collect()
            };
            if displayed.is_empty() {
                continue;
            }
            let phase_sec = preview.section(phase_item.name.display_name());
            for item in displayed {
                phase_sec.bullet(item);
            }
        }
        for w in &plan.warnings {
            preview.status_simple(Role::Warn, w);
        }
    }

    // Confirm
    if !yes {
        // Closed-TTY / non-interactive defaults to "no" — apply is destructive
        // and silence is treated as decline, not as approval.
        let confirmed = printer
            .prompt_confirm("Apply these changes?")
            .unwrap_or(false);
        if !confirmed {
            printer.status_simple(Role::Info, "Aborted");
            printer.emit(Doc::new().with_data(ApplyOutput::aborted()));
            return Ok(ApplyOutcome::success());
        }
    }

    // Acquire apply lock to prevent concurrent applies
    let lock_state_dir;
    let apply_lock_dir: &std::path::Path = if let Some(dir) = cli.state_dir.as_deref() {
        dir
    } else {
        lock_state_dir = cfgd_core::state::default_state_dir()
            .map_err(|e| anyhow::anyhow!("cannot determine state directory: {}", e))?;
        &lock_state_dir
    };
    let _apply_lock = cfgd_core::acquire_apply_lock(apply_lock_dir)?;

    // Register cooperative-cancellation handlers for the duration of the apply.
    // SIGINT/SIGTERM flip the shared flag (the reconciler checks it between
    // atomic actions); the in-flight action still finishes, so no file is torn.
    let abort = cfgd_core::AbortFlag::new();
    register_abort_handlers(&abort);

    // Apply
    let shell_override = args.shell.map(super::apply_shell_to_script_shell);
    let result = reconciler.apply(
        &plan,
        &effective_resolved,
        &config_dir,
        printer,
        phase_filter.as_ref(),
        &resolved_modules,
        reconcile_context,
        args.skip_scripts,
        shell_override,
        &abort,
    )?;

    if let Some(code) = result.aborted {
        let signal = if code == 143 { "SIGTERM" } else { "SIGINT" };
        let applied = result.succeeded();
        // Filter-aware planned count (computed by the reconciler with the same
        // predicate the apply loop uses), so "{applied} of {total}" reflects
        // only the in-scope actions under --phase/--skip/--only, not the whole
        // plan.
        let total = result.planned_total;
        printer.emit(
            Doc::new()
                .status(
                    Role::Warn,
                    format!(
                        "apply aborted by signal — {applied} of {total} action(s) applied; no partial writes, rerun to converge"
                    ),
                )
                .with_data(AbortOutput {
                    aborted: true,
                    signal: signal.to_string(),
                    applied,
                    total,
                }),
        );
        return Ok(ApplyOutcome {
            status: result.status,
            aborted_code: Some(code),
        });
    }

    let status = print_apply_result(&result, printer, Some(start.elapsed()));

    // Link source commits to this apply for provenance tracking
    if !source_commits.is_empty() {
        for (source_name, commit_hash) in &source_commits {
            if let Err(e) = state.record_source_apply(source_name, result.apply_id, commit_hash) {
                tracing::warn!(
                    source = %source_name,
                    commit = %commit_hash,
                    error = %e,
                    "failed to record source apply"
                );
            }
        }
    }

    // Prune old backups — keep last 10 applies' worth. Best-effort: failures
    // here (SQLite locked, disk full, permission denied) are surfaced as a
    // warn-level log so unbounded backup growth on a stuck filesystem is
    // observable instead of silent.
    if let Err(e) = state.prune_old_backups(10) {
        tracing::warn!(error = %e, "failed to prune old backups");
    }

    let output = ApplyOutput {
        status: apply_status_str(&status).to_string(),
        apply_id: Some(result.apply_id),
        succeeded: result.succeeded(),
        failed: result.failed(),
        source_commits,
    };
    printer.emit(Doc::new().with_data(&output));

    Ok(ApplyOutcome {
        status,
        aborted_code: None,
    })
}

/// Structured `-o json` payload emitted when an apply is cooperatively aborted
/// by a signal.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct AbortOutput {
    aborted: bool,
    signal: String,
    applied: usize,
    total: usize,
}

/// Register SIGINT (→130) / SIGTERM (→143) handlers for the running apply.
///
/// The FIRST delivery of a signal flips `abort` (the reconciler stops
/// cooperatively between atomic actions). A SECOND delivery of the same signal
/// force-quits via the OS default disposition, so a user hammering Ctrl-C is
/// never stuck waiting on cleanup.
///
/// Both the flag store and the default-disposition emulation are async-signal-
/// safe (`AtomicUsize::store` / `signal_hook::low_level::emulate_default_handler`).
/// Best-effort — a registration failure is logged and the apply proceeds
/// without cooperative cancellation (the OS default still terminates on signal).
#[cfg(unix)]
fn register_abort_handlers(abort: &cfgd_core::AbortFlag) {
    use std::sync::atomic::Ordering;

    use signal_hook::consts::{SIGINT, SIGTERM};
    use signal_hook::low_level;

    // 128 + signum, the POSIX shell convention for signal-terminated processes.
    for (sig, code) in [(SIGINT, 130usize), (SIGTERM, 143usize)] {
        let flag = abort.raw();
        // SAFETY: the action only performs async-signal-safe operations — an
        // atomic load/store and `emulate_default_handler` (documented
        // async-signal-safe). No allocation, locking, or reentrant I/O.
        let res = unsafe {
            low_level::register(sig, move || {
                if flag.load(Ordering::SeqCst) == 0 {
                    // First delivery: request cooperative cancellation.
                    flag.store(code, Ordering::SeqCst);
                } else {
                    // Second delivery: force-quit with the default disposition.
                    let _ = low_level::emulate_default_handler(sig);
                }
            })
        };
        if let Err(e) = res {
            tracing::warn!(signal = sig, error = %e, "failed to register apply abort handler");
        }
    }
}

/// Windows lacks `signal_hook`'s POSIX flag API; cooperative abort is a no-op
/// here and Ctrl-C falls back to the OS default disposition. Logged so the
/// degraded behavior is observable.
#[cfg(windows)]
fn register_abort_handlers(_abort: &cfgd_core::AbortFlag) {
    tracing::debug!("cooperative apply abort handler not available on this platform");
}

/// Remove package-tracking rows whose package is no longer installed (stale
/// after a partial-uninstall failure or an out-of-band removal). Best-effort:
/// any failure is logged, never propagated, so it can't fail an apply.
fn gc_stale_package_tracking(
    state: &cfgd_core::state::StateStore,
    managers: &[&dyn cfgd_core::providers::PackageManager],
) {
    let tracked = match cfgd_installed_packages(state) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "failed to read tracked packages for GC");
            return;
        }
    };
    match cfgd_core::reconciler::stale_tracked_packages(managers, &tracked) {
        Ok(stale) => {
            for (mgr, id) in stale {
                let rid = format!("{mgr}/{id}");
                if let Err(e) = state.remove_managed_resource("package", &rid) {
                    tracing::warn!(resource = %rid, error = %e, "failed to GC stale package tracking row");
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, "failed to compute stale package tracking rows"),
    }
}

/// Run persisted uninstall scripts for orphaned custom-manager packages, then
/// drop the rows that were removed successfully. Best-effort: store errors are
/// logged, never propagated, so the GC can't fail an apply. Mirrors
/// [`gc_stale_package_tracking`].
fn gc_orphaned_custom_packages(
    state: &cfgd_core::state::StateStore,
    registry: &cfgd_core::providers::ProviderRegistry,
    printer: &cfgd_core::output::Printer,
) {
    let known = registry.manager_names();
    let orphans = match state.orphaned_package_resources(&known) {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(error = %e, "failed to read orphaned package rows for GC");
            return;
        }
    };
    if orphans.is_empty() {
        return;
    }
    for (mgr, pkg) in packages::prune_orphaned_packages(&orphans, printer) {
        let rid = format!("{mgr}/{pkg}");
        if let Err(e) = state.remove_managed_resource("package", &rid) {
            tracing::warn!(resource = %rid, error = %e, "failed to GC orphaned package tracking row");
        }
    }
}

/// Preview, without executing, which orphaned custom-manager packages a real
/// apply would prune via their persisted uninstall script — and which lack one
/// and need manual removal. Read-only; runs only on the dry-run path.
pub(in crate::cli) fn preview_orphaned_custom_packages(
    state: &cfgd_core::state::StateStore,
    registry: &cfgd_core::providers::ProviderRegistry,
    printer: &cfgd_core::output::Printer,
) {
    let known = registry.manager_names();
    let orphans = match state.orphaned_package_resources(&known) {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(error = %e, "failed to read orphaned package rows for preview");
            return;
        }
    };
    for orphan in orphans {
        match orphan.uninstall_cmd {
            Some(_) => printer.status_simple(
                Role::Accent,
                format!(
                    "would uninstall orphaned {}/{} via persisted script",
                    orphan.manager, orphan.package
                ),
            ),
            None => printer.status_simple(
                Role::Warn,
                format!(
                    "orphaned {}/{} — no persisted uninstall; manual removal needed",
                    orphan.manager, orphan.package
                ),
            ),
        }
    }
}

fn apply_status_str(status: &cfgd_core::state::ApplyStatus) -> &'static str {
    match status {
        cfgd_core::state::ApplyStatus::Success => "success",
        cfgd_core::state::ApplyStatus::Partial => "partial",
        cfgd_core::state::ApplyStatus::Failed => "failed",
        cfgd_core::state::ApplyStatus::InProgress => "inProgress",
        cfgd_core::state::ApplyStatus::Aborted => "aborted",
    }
}

/// Build the buffered `Doc` that carries the final `ApplyOutput` payload.
/// Pure function so snapshot tests can drive the JSON path without standing
/// up a reconciler.
pub fn build_apply_doc(output: &ApplyOutput) -> Doc {
    Doc::new().with_data(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cfgd_core::output::{Printer, Verbosity, strip_ansi};

    #[test]
    fn preview_orphaned_custom_packages_pins_both_contract_strings_and_executes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join("preview-should-not-run");

        let state = cfgd_core::state::StateStore::open_in_memory().unwrap();
        // Orphan WITH a persisted script: would create the marker file if the
        // preview ever executed it (it must not).
        state
            .upsert_package_resource(
                "widgetmgr/widget",
                "local",
                None,
                Some(&format!("touch {}", marker.display())),
            )
            .unwrap();
        // Orphan with NO persisted script (legacy row).
        state
            .upsert_package_resource("legacymgr/legacypkg", "local", None, None)
            .unwrap();
        // A package under a registered (built-in) manager — NOT orphaned.
        state
            .upsert_package_resource("cargo/bat", "local", None, None)
            .unwrap();

        // Registry contains only built-in managers, so cargo is "known" but
        // widgetmgr / legacymgr are not — exactly the orphan condition.
        let mut registry = cfgd_core::providers::ProviderRegistry::new();
        registry.package_managers = crate::packages::all_package_managers();

        // Normal verbosity: Accent/Warn status lines are suppressed under Quiet
        // (the default for `for_test`).
        let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
        preview_orphaned_custom_packages(&state, &registry, &printer);
        drop(printer);
        let out = strip_ansi(&buf.lock().unwrap());

        assert!(
            out.contains("would uninstall orphaned widgetmgr/widget via persisted script"),
            "persisted-script preview line missing, got: {out}"
        );
        assert!(
            out.contains(
                "orphaned legacymgr/legacypkg — no persisted uninstall; manual removal needed"
            ),
            "no-persisted-script preview line missing, got: {out}"
        );
        assert!(
            !out.contains("cargo/bat"),
            "a package under a registered manager must not be previewed as orphaned, got: {out}"
        );

        // Preview is read-only: the script was never executed and no row was
        // removed.
        assert!(
            !marker.exists(),
            "preview must not execute any uninstall script"
        );
        let known = registry.manager_names();
        let still_orphaned = state.orphaned_package_resources(&known).unwrap();
        assert_eq!(
            still_orphaned.len(),
            2,
            "preview must not remove any tracking rows"
        );
    }
}
