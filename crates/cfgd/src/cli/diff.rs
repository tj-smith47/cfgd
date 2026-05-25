use super::*;

use cfgd_core::output::{Doc, Printer, Role, section_guard::SectionGuard};

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

    let (_cfg, profile_name, mut resolved) = load_config_and_profile(cli)?;
    printer.kv_block([
        ("Config".to_string(), cli.config.display().to_string()),
        ("Profile".to_string(), profile_name),
    ]);

    packages::resolve_manifest_packages(&mut resolved.merged.packages, &config_dir)?;

    let registry = build_registry_with_profile(&resolved.merged.packages);

    let mut diff_payload = DiffOutput::default();
    let mut has_system_drift = false;

    let has_file_drift = {
        printer.status_simple(Role::Info, "Files");
        let fm = CfgdFileManager::new(&config_dir, &resolved)?;
        let drift = fm.diff(&resolved.merged, printer)?;
        if drift {
            printer.status_simple(Role::Warn, "File drift detected");
        } else {
            printer.status_simple(Role::Ok, "No file drift");
        }
        drift
    };

    let has_pkg_drift = {
        let pkg_sec = printer.section("Packages");
        let all_managers: Vec<&dyn cfgd_core::providers::PackageManager> = registry
            .package_managers
            .iter()
            .map(|m| m.as_ref())
            .collect();
        let pkg_actions = packages::plan_packages(&resolved.merged, &all_managers)?;
        print_package_drift(&pkg_actions, &pkg_sec, &mut diff_payload)
    };

    {
        let sys_sec = printer.section("System");
        let available_configurators = registry.available_system_configurators();
        for configurator in &available_configurators {
            let key = configurator.name();
            let desired = match resolved.merged.system.get(key) {
                Some(v) => v,
                None => continue,
            };
            match configurator.diff(desired) {
                Ok(drifts) if !drifts.is_empty() => {
                    has_system_drift = true;
                    for drift in &drifts {
                        sys_sec
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
                    sys_sec
                        .status(Role::Warn, format!("{}: error checking drift", key))
                        .detail(e.to_string());
                }
                _ => {}
            }
        }
        if !has_system_drift {
            sys_sec.status_simple(Role::Ok, "No system drift");
        }
    }

    diff_payload.summary = DiffSummary {
        has_file_drift,
        has_pkg_drift,
        has_system_drift,
    };

    printer.emit(build_diff_doc(&diff_payload));

    if exit_code && (has_file_drift || has_pkg_drift || has_system_drift) {
        cfgd_core::exit::ExitCode::DriftDetected.exit();
    }

    Ok(())
}

fn cmd_diff_module(
    _cli: &Cli,
    printer: &Printer,
    mod_name: &str,
    config_dir: &std::path::Path,
    exit_code: bool,
) -> anyhow::Result<()> {
    let registry = build_registry();
    let platform = Platform::detect();
    let mgr_map = managers_map(&registry);
    let cache_base = modules::default_module_cache_dir()?;
    let resolved_modules = match modules::resolve_modules(
        &[mod_name.to_string()],
        config_dir,
        &cache_base,
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

    let mut diff_payload = DiffOutput::default();
    let mut has_file_diff = false;
    let mut has_pkg_drift = false;

    {
        let files_sec = printer.section("Files");
        for module in &resolved_modules {
            for file in &module.files {
                if file.target.exists() {
                    if file.source.exists() {
                        let source_content =
                            std::fs::read_to_string(&file.source).unwrap_or_default();
                        let target_content =
                            std::fs::read_to_string(&file.target).unwrap_or_default();
                        if source_content != target_content {
                            has_file_diff = true;
                            files_sec
                                .status(Role::Warn, format!("{}", file.target.display()))
                                .detail("content differs");
                            printer.diff(&target_content, &source_content);
                        }
                    }
                } else {
                    has_file_diff = true;
                    files_sec
                        .status(Role::Warn, format!("{}", file.target.display()))
                        .detail("missing");
                }
            }
        }
        if !has_file_diff {
            files_sec.status_simple(Role::Ok, "No file drift");
        }
    }

    {
        let pkg_sec = printer.section("Packages");
        let mut emitted = false;
        for module in &resolved_modules {
            for pkg in &module.packages {
                if pkg.manager == "script" {
                    continue;
                }
                if let Some(mgr) = mgr_map.get(pkg.manager.as_str()) {
                    let installed = mgr.installed_packages().unwrap_or_default();
                    if !installed.contains(&pkg.resolved_name) {
                        has_pkg_drift = true;
                        emitted = true;
                        pkg_sec
                            .status(Role::Warn, format!("{}: missing", pkg.manager))
                            .detail(pkg.resolved_name.clone());
                        diff_payload.packages.push(PackageDrift {
                            manager: pkg.manager.clone(),
                            shape: "missing".to_string(),
                            packages: vec![pkg.resolved_name.clone()],
                            bootstrap_method: None,
                        });
                    }
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
        has_system_drift: false,
    };

    printer.emit(build_diff_doc(&diff_payload));

    if exit_code && (has_file_diff || has_pkg_drift) {
        cfgd_core::exit::ExitCode::DriftDetected.exit();
    }

    Ok(())
}

fn print_package_drift(
    pkg_actions: &[PackageAction],
    section: &SectionGuard<'_>,
    payload: &mut DiffOutput,
) -> bool {
    let pkg_diffs: Vec<&PackageAction> = pkg_actions
        .iter()
        .filter(|a| !matches!(a, PackageAction::Skip { .. }))
        .collect();
    let has_drift = !pkg_diffs.is_empty();
    if pkg_diffs.is_empty() {
        section.status_simple(Role::Ok, "No package drift");
    } else {
        for action in &pkg_diffs {
            match action {
                PackageAction::Bootstrap {
                    manager, method, ..
                } => {
                    section
                        .status(Role::Warn, format!("{}: not installed", manager))
                        .detail(format!("can bootstrap via {}", method));
                    payload.packages.push(PackageDrift {
                        manager: manager.clone(),
                        shape: "bootstrap".to_string(),
                        packages: Vec::new(),
                        bootstrap_method: Some(method.clone()),
                    });
                }
                PackageAction::Install {
                    manager, packages, ..
                } => {
                    section
                        .status(Role::Warn, format!("{}: missing", manager))
                        .detail(packages.join(", "));
                    payload.packages.push(PackageDrift {
                        manager: manager.clone(),
                        shape: "missing".to_string(),
                        packages: packages.clone(),
                        bootstrap_method: None,
                    });
                }
                PackageAction::Uninstall {
                    manager, packages, ..
                } => {
                    section
                        .status(Role::Warn, format!("{}: extra", manager))
                        .detail(packages.join(", "));
                    payload.packages.push(PackageDrift {
                        manager: manager.clone(),
                        shape: "extra".to_string(),
                        packages: packages.clone(),
                        bootstrap_method: None,
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
    let role = if any_drift { Role::Warn } else { Role::Ok };
    let subject = if any_drift {
        "Drift detected"
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
            let section = printer.section("Packages");
            let has_drift = print_package_drift(&actions, &section, &mut payload);
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

    #[test]
    fn print_package_drift_skip_only_is_ignored_when_mixed_with_other_actions() {
        // Covers PackageAction::Skip arm at L276 — a Skip mixed in with real
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
            let section = printer.section("Packages");
            let has_drift = print_package_drift(&actions, &section, &mut payload);
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
            PackageAction::Bootstrap {
                manager: "pipx".into(),
                method: "pip install pipx".into(),
                origin: "profile".into(),
            },
        ];
        {
            let section = printer.section("Packages");
            let has_drift = print_package_drift(&actions, &section, &mut payload);
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
            output.contains("pipx: not installed") && output.contains("bootstrap"),
            "should show bootstrap need, got: {output}"
        );
        assert_eq!(payload.packages.len(), 3);
    }
}
