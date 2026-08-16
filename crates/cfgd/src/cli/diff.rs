use super::*;

use cfgd_core::PathDisplayExt;
use cfgd_core::output::{Doc, OwnerLabel, Printer, Role, section_guard::SectionGuard};
use cfgd_core::reconciler::{MANAGERS_GROUP, ManagerAction, Owner, PhaseName};

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
        None => Ok(fm.diff_one(&file.source, &file.target, None, printer)?),
    }
}

/// Keep only the records worth reporting: a converged file is the absence of a
/// finding, and listing every one of them would bury the drifted and the
/// unevaluable entries a consumer actually acts on.
fn record_file_drift(
    payload: &mut DiffOutput,
    record: cfgd_core::providers::FileDriftResult,
) -> bool {
    let drifted = !record.matches;
    if drifted {
        payload.files.push(record);
    }
    drifted
}

/// The owner's group heading, from the one token constructor.
fn owner_label(owner: &Owner) -> OwnerLabel {
    OwnerLabel::new(owner.kind.as_str(), owner.name.as_str())
}

/// Render a file group's status lines as they happen instead of buffering them
/// until the group closes.
///
/// Every file's status line is followed by its own raw content block — a
/// unified diff or a highlighted body — and the renderer never buffers those.
/// Buffered statuses would flush at group close, printing every diff body above
/// the line that names the file it belongs to. Nothing in a file group carries
/// a trailing field, so the alignment a buffered group would buy is zero.
fn live_file_group(group: &SectionGuard<'_>) {
    group.live_column(0);
}

pub fn cmd_diff(
    cli: &Cli,
    printer: &Printer,
    module_filter: Option<&str>,
    exit_code: bool,
) -> anyhow::Result<()> {
    printer.heading("Diff");

    let config_dir = config_dir(cli);

    if let Some(mod_name) = module_filter {
        return cmd_diff_module(cli, printer, mod_name, &config_dir, exit_code);
    }

    let (cfg, profile_name, local_resolved) = load_config_and_profile(cli, printer)?;
    printer.kv_block([
        ("Config".to_string(), cli.config.display_posix()),
        ("Profile".to_string(), profile_name.clone()),
    ]);
    // Drift is reported under the same owner that would be named in the plan
    // that fixes it, so the two surfaces read as one coordinate system.
    let profile_owner = Owner::profile(profile_name);

    // Compose with sources (cache-only — read paths stay offline) and resolve the
    // effective module set through the one shared resolver, so `diff` sees the
    // same source-composed desired state that `apply` writes.
    let desired = resolve_desired_state(
        cli,
        &cfg,
        &local_resolved,
        None,
        printer,
        false,
        composition::ConstraintMode::Report,
    )?;
    let mut resolved = desired.resolved;
    let resolved_modules = desired.modules;

    packages::resolve_manifest_packages(&mut resolved.merged.packages, &config_dir)?;

    let registry = build_registry_with_profile(&resolved.merged.packages);

    let mut diff_payload = DiffOutput::default();
    let mut has_system_drift = false;

    let has_file_drift = {
        let files_phase = printer.section_phase(&PhaseName::Files.section_label());
        // The file renderers take a bare `&Printer` and know nothing of the
        // tree; depth inheritance is what lands their per-file lines inside
        // the owner group opened around them.
        let _inherit = printer.depth_inheritance();
        let fm = CfgdFileManager::new(&config_dir, &resolved)?;
        let mut drift = false;
        {
            let group = files_phase.section_owner_or_collapse(&owner_label(&profile_owner));
            live_file_group(&group);
            for record in fm.diff(&resolved.merged, printer)? {
                drift |= record_file_drift(&mut diff_payload, record);
            }
        }
        // Module-deployed files render the same inline content diff as profile
        // files (module sources carry no tera origin, so pass None).
        for module in &resolved_modules {
            let group =
                files_phase.section_owner_or_collapse(&OwnerLabel::new("module", &module.name));
            live_file_group(&group);
            for file in &module.files {
                let record = diff_module_file(&fm, &resolved, module, file, &config_dir, printer)?;
                if record_file_drift(&mut diff_payload, record) {
                    drift = true;
                }
            }
        }
        if drift {
            files_phase.status_simple(Role::Warn, "File drift detected");
        } else {
            files_phase.status_simple(Role::Ok, "No file drift");
        }
        drift
    };

    let has_pkg_drift = {
        let pkg_sec = printer.section_phase(&PhaseName::Packages.section_label());
        let all_managers: Vec<&dyn cfgd_core::providers::PackageManager> = registry
            .package_managers
            .iter()
            .map(|m| m.as_ref())
            .collect();
        // Tracked-but-dropped packages must surface as drift here, so read the
        // cfgd-installed set from state to bound prune the same way apply does.
        let state = open_state_store(cli.state_dir.as_deref(), cli.scope())?;
        let cfgd_installed = cfgd_installed_packages(&state)?;
        let pkg_cx = cfgd_core::providers::PackageContext::new(printer, &state);
        let pkg_actions = packages::plan_packages(
            &resolved.merged,
            &resolved_modules,
            &all_managers,
            &cfgd_installed,
            &pkg_cx,
        )?;
        // Same planner the Prerequisites phase runs, so a manager `diff` calls
        // out as drift is exactly the one `apply` would provision or refuse —
        // and the same predicate `verify`/`status -e` share via
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

    {
        let sys_sec = printer.section_phase(&PhaseName::System.section_label());
        // Every system key resolves against the merged profile ⊕ module view,
        // which is what puts a system action under the profile owner in the
        // plan too (`owner_of`'s fall-through arm).
        {
            let sys_group = sys_sec.section_owner_or_collapse(&owner_label(&profile_owner));
            let available_configurators = registry.available_system_configurators();
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
                    Ok(drifts) if !drifts.is_empty() => {
                        has_system_drift = true;
                        for drift in &drifts {
                            sys_group
                                .status(Role::Warn, format!("{}.{}", key, drift.key))
                                .detail(format!("want {}, have {}", drift.expected, drift.actual));
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
                            .status(Role::Warn, format!("{}: error checking drift", key))
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
        close_system_phase(&sys_sec, has_system_drift, diff_payload.system_errors.len());
    }

    diff_payload.summary = DiffSummary {
        has_file_drift,
        has_pkg_drift,
        has_system_drift,
        system_check_failed: !diff_payload.system_errors.is_empty(),
    };

    printer.emit(build_diff_doc(&diff_payload));

    if exit_code && let Some(code) = diff_exit_code(&diff_payload.summary) {
        code.exit();
    }

    Ok(())
}

/// The System phase's closing line.
///
/// A configurator whose check ERRORED has already spoken inside its owner
/// group; what the phase must not then do is close with `No system drift`. A
/// check that could not run is not a check that passed, and the group above it
/// makes the contradiction plain.
fn close_system_phase(sec: &SectionGuard<'_>, drift: bool, unchecked: usize) {
    if unchecked > 0 {
        sec.status(Role::Warn, "System drift undetermined")
            .detail(format!(
                "{} could not be checked",
                cfgd_core::pluralize(unchecked, "configurator")
            ));
    } else if !drift {
        sec.status_simple(Role::Ok, "No system drift");
    }
}

/// What `--exit-code` reports, from the same summary the `-o json` payload
/// carries — so the exit status and the payload can never disagree about
/// whether this machine is in sync.
///
/// A failed check outranks drift: `DriftDetected` tells a script the machine
/// needs an apply, while a check that could not run means the answer is
/// unknown, which is an error rather than a verdict.
fn diff_exit_code(summary: &DiffSummary) -> Option<cfgd_core::exit::ExitCode> {
    if summary.system_check_failed {
        return Some(cfgd_core::exit::ExitCode::Error);
    }
    (summary.has_file_drift || summary.has_pkg_drift || summary.has_system_drift)
        .then_some(cfgd_core::exit::ExitCode::DriftDetected)
}

fn cmd_diff_module(
    cli: &Cli,
    printer: &Printer,
    mod_name: &str,
    config_dir: &std::path::Path,
    exit_code: bool,
) -> anyhow::Result<()> {
    let registry = build_registry();
    let platform = Platform::detect();
    let mgr_map = managers_map(&registry);
    let cache_base = module_cache_dir(cli)?;
    let resolved_modules = match modules::resolve_modules(
        &[mod_name.to_string()],
        config_dir,
        &cache_base,
        &[],
        &platform,
        &mgr_map,
        printer,
    ) {
        Ok(mods) => mods,
        Err(_) => {
            printer.emit(
                Doc::new()
                    .status(
                        Role::Info,
                        format!("Module '{}' not found — nothing to diff", mod_name),
                    )
                    .with_data(DiffOutput::default()),
            );
            return Ok(());
        }
    };

    printer.kv_block([("Module".to_string(), mod_name.to_string())]);

    let state = open_state_store(cli.state_dir.as_deref(), cli.scope())?;
    let pkg_cx = cfgd_core::providers::PackageContext::new(printer, &state);

    let mut diff_payload = DiffOutput::default();
    let mut has_file_diff = false;
    let mut has_pkg_drift = false;

    {
        // Mirror the full `cmd_diff` path: the `Phase: Files` heading, one
        // `module:<name>` group per module, the shared per-file inline-diff
        // renderer, then the phase's summary line. Module sources carry no
        // tera origin (None).
        let files_phase = printer.section_phase(&PhaseName::Files.section_label());
        let _inherit = printer.depth_inheritance();
        let resolved = empty_resolved_profile(mod_name, &active_profile_name(cli, None));
        let fm = CfgdFileManager::new(config_dir, &resolved)?;
        for module in &resolved_modules {
            let group =
                files_phase.section_owner_or_collapse(&OwnerLabel::new("module", &module.name));
            live_file_group(&group);
            for file in &module.files {
                let record = diff_module_file(&fm, &resolved, module, file, config_dir, printer)?;
                if record_file_drift(&mut diff_payload, record) {
                    has_file_diff = true;
                }
            }
        }
        if has_file_diff {
            files_phase.status_simple(Role::Warn, "File drift detected");
        } else {
            files_phase.status_simple(Role::Ok, "No file drift");
        }
    }

    {
        let pkg_sec = printer.section_phase(&PhaseName::Packages.section_label());
        let mut emitted = false;
        for module in &resolved_modules {
            let group = pkg_sec.section_owner_or_collapse(&OwnerLabel::new("module", &module.name));
            for pkg in &module.packages {
                if let Some(drift) = package_missing_drift(pkg, &mgr_map, &pkg_cx) {
                    has_pkg_drift = true;
                    emitted = true;
                    group
                        .status(Role::Warn, format!("{}: missing", pkg.manager))
                        .detail(pkg.resolved_name.clone());
                    diff_payload.packages.push(drift);
                }
            }
        }
        if !emitted {
            pkg_sec.status_simple(Role::Ok, "No package drift");
        }
    }

    diff_payload.summary = DiffSummary {
        has_file_drift: has_file_diff,
        has_pkg_drift,
        // A single module's diff evaluates no system configurator, so the
        // system verdict is neither drifted nor undetermined here.
        has_system_drift: false,
        system_check_failed: false,
    };

    printer.emit(build_diff_doc(&diff_payload));

    if exit_code && let Some(code) = diff_exit_code(&diff_payload.summary) {
        code.exit();
    }

    Ok(())
}

/// Drift record for a module-declared package that is not installed, or `None`
/// when it is installed, script-based, or its manager isn't registered.
/// The comparison routes through `package_identity` so case-insensitive managers
/// (choco/scoop/winget) and name-remapping ones (go) match installed state like
/// with like — a raw name compare re-reports installed packages as missing on
/// every `cfgd diff --module`.
fn package_missing_drift(
    pkg: &modules::ResolvedPackage,
    mgr_map: &std::collections::HashMap<String, &dyn cfgd_core::providers::PackageManager>,
    cx: &cfgd_core::providers::PackageContext<'_>,
) -> Option<PackageDrift> {
    if pkg.manager == "script" {
        return None;
    }
    let mgr = mgr_map.get(pkg.manager.as_str())?;
    let installed = mgr.installed_packages(cx).unwrap_or_default();
    if installed.contains(&mgr.package_identity(&pkg.resolved_name)) {
        return None;
    }
    Some(PackageDrift {
        manager: pkg.manager.clone(),
        shape: "missing".to_string(),
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
    let pkg_diffs: Vec<&PackageAction> = pkg_actions
        .iter()
        .filter(|a| !matches!(a, PackageAction::Skip { .. }))
        .collect();
    let has_drift = !pkg_diffs.is_empty() || !manager_actions.is_empty();
    if !has_drift {
        section.status_simple(Role::Ok, "No package drift");
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
                // `status -e` fold into their own rows, so the two surfaces
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
                                .status(Role::Warn, format!("{}: {}", manager, phrase.state))
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
                            .status(Role::Warn, format!("{}: {}", manager, phrase.state))
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
                        .status(Role::Warn, format!("{}: missing", manager))
                        .detail(packages.join(", "));
                    payload.packages.push(PackageDrift {
                        manager: manager.clone(),
                        shape: "missing".to_string(),
                        packages: packages.clone(),
                        bootstrap_method: None,
                        reason: None,
                    });
                }
                PackageAction::Uninstall {
                    manager, packages, ..
                } => {
                    group
                        .status(Role::Warn, format!("{}: extra", manager))
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

pub fn build_diff_doc(output: &DiffOutput) -> Doc {
    let any_drift = output.summary.has_file_drift
        || output.summary.has_pkg_drift
        || output.summary.has_system_drift;
    // A run that could not check everything has no clean verdict to give, so
    // it never renders one — whether or not the checks that DID run found
    // drift.
    let role = if any_drift || output.summary.system_check_failed {
        Role::Warn
    } else {
        Role::Ok
    };
    let subject = if any_drift {
        "Drift detected"
    } else if output.summary.system_check_failed {
        "Drift undetermined — a system check could not run"
    } else {
        "No drift detected"
    };
    Doc::new().status(role, subject).with_data(output)
}

#[cfg(test)]
mod tests {
    use super::*;

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
            let section = printer.section_phase(&PhaseName::Packages.section_label());
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
            output.contains("No package drift"),
            "all-skip should show no drift, got: {output}"
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
            },
            ..Default::default()
        };
        let (printer, cap) = Printer::for_test_doc();
        printer.emit(build_diff_doc(&payload));
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
            },
            ..Default::default()
        };
        let (printer, cap) = Printer::for_test_doc();
        printer.emit(build_diff_doc(&payload));
        drop(printer);
        let out = strip_ansi(&cap.human());
        assert!(
            out.contains("Drift detected"),
            "drift doc must surface warning: {out}"
        );
    }

    /// A configurator whose check errored leaves the machine's state unknown.
    /// All three of the command's answers — the human phase line, the `-o json`
    /// summary and the `--exit-code` status — must say so, because a script
    /// that reads any one of them as "clean" acts on a check that never ran.
    #[test]
    fn a_failed_system_check_is_never_reported_as_clean() {
        let (printer, buf) = Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
        {
            let sec = printer.section_phase(&PhaseName::System.section_label());
            close_system_phase(&sec, false, 1);
        }
        drop(printer);
        let human = strip_ansi(&buf.lock().expect("capture").clone());
        assert!(
            !human.contains("No system drift"),
            "a check that could not run is not a check that passed: {human}"
        );
        assert!(
            human.contains("System drift undetermined"),
            "the phase must name the gap: {human}"
        );

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
            },
            ..Default::default()
        };
        let (printer, cap) = Printer::for_test_doc();
        printer.emit(build_diff_doc(&payload));
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
    }

    #[test]
    fn a_clean_run_reports_its_clean_verdict_on_every_channel() {
        let (printer, buf) = Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
        {
            let sec = printer.section_phase(&PhaseName::System.section_label());
            close_system_phase(&sec, false, 0);
        }
        drop(printer);
        let human = strip_ansi(&buf.lock().expect("capture").clone());
        assert!(
            human.contains("No system drift"),
            "an all-checks-ran, no-drift phase still says so"
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
            let section = printer.section_phase(&PhaseName::Packages.section_label());
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
            let section = printer.section_phase(&PhaseName::Packages.section_label());
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
            output.contains("cargo: missing") && output.contains("ripgrep"),
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
                batched: vec![],
                depends_on: vec![],
            },
            ManagerAction::Refuse {
                manager: "snap".into(),
                reason: "no available system manager".into(),
            },
        ];
        {
            let section = printer.section_phase(&PhaseName::Packages.section_label());
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
            version: None,
            script: None,
            creates: None,
            only_if: None,
            unless: None,
        }
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
