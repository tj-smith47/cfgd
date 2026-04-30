use super::*;

pub(super) fn cmd_diff(
    cli: &Cli,
    printer: &Printer,
    module_filter: Option<&str>,
    exit_code: bool,
) -> anyhow::Result<()> {
    printer.header("Diff");

    let config_dir = config_dir(cli);

    if let Some(mod_name) = module_filter {
        // Module-only diff: load just that module's files and packages
        let registry = build_registry();
        let platform = Platform::detect();
        let mgr_map = managers_map(&registry);
        let cache_base = modules::default_module_cache_dir()?;
        let resolved_modules = match modules::resolve_modules(
            &[mod_name.to_string()],
            &config_dir,
            &cache_base,
            &platform,
            &mgr_map,
            printer,
        ) {
            Ok(mods) => mods,
            Err(_) => {
                printer.info(&format!(
                    "Module '{}' not found — nothing to diff",
                    mod_name
                ));
                return Ok(());
            }
        };

        printer.key_value("Module", mod_name);
        printer.newline();

        // Module file diffs
        printer.subheader("Files");
        let mut has_file_diff = false;
        for module in &resolved_modules {
            for file in &module.files {
                if file.target.exists() {
                    if file.source.exists() {
                        // diff is best-effort visualization; an unreadable file
                        // shows as empty string here so verify/apply (which DO
                        // surface read errors) remain the authoritative drift path.
                        let source_content =
                            std::fs::read_to_string(&file.source).unwrap_or_default();
                        let target_content =
                            std::fs::read_to_string(&file.target).unwrap_or_default();
                        if source_content != target_content {
                            has_file_diff = true;
                            printer.info(&format!("{}:", file.target.display()));
                            printer.diff(&target_content, &source_content);
                        }
                    }
                } else {
                    has_file_diff = true;
                    printer.warning(&format!("{}: missing", file.target.display()));
                }
            }
        }
        if !has_file_diff {
            printer.success("No file drift");
        }

        // Module package diffs — check if resolved packages are installed
        printer.newline();
        printer.subheader("Packages");
        let mut has_pkg_drift = false;
        for module in &resolved_modules {
            for pkg in &module.packages {
                if pkg.manager == "script" {
                    continue;
                }
                if let Some(mgr) = mgr_map.get(pkg.manager.as_str()) {
                    // If a manager fails to enumerate (e.g., not available on
                    // this host), treat as empty so diff doesn't error out;
                    // apply/verify surface the underlying failure.
                    let installed = mgr.installed_packages().unwrap_or_default();
                    if !installed.contains(&pkg.resolved_name) {
                        has_pkg_drift = true;
                        printer
                            .warning(&format!("{}: missing — {}", pkg.manager, pkg.resolved_name));
                    }
                }
            }
        }
        if !has_pkg_drift {
            printer.success("No package drift");
        }

        if exit_code && (has_file_diff || has_pkg_drift) {
            cfgd_core::exit::ExitCode::DriftDetected.exit();
        }

        return Ok(());
    }

    let (_cfg, mut resolved) = load_config_and_profile(cli, printer)?;

    // Resolve manifest files
    packages::resolve_manifest_packages(&mut resolved.merged.packages, &config_dir)?;

    let registry = build_registry_with_profile(&resolved.merged.packages);

    printer.newline();

    // File diffs
    printer.subheader("Files");
    let fm = CfgdFileManager::new(&config_dir, &resolved)?;
    let has_file_diff = fm.diff(&resolved.merged, printer)?;

    // Package drift
    printer.newline();
    printer.subheader("Packages");
    let all_managers: Vec<&dyn cfgd_core::providers::PackageManager> = registry
        .package_managers
        .iter()
        .map(|m| m.as_ref())
        .collect();
    let pkg_actions = packages::plan_packages(&resolved.merged, &all_managers)?;
    let has_pkg_drift = print_package_drift(&pkg_actions, printer);

    // System drift
    printer.newline();
    printer.subheader("System");
    let available_configurators = registry.available_system_configurators();
    let mut has_system_drift = false;
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
                    printer.warning(&format!(
                        "{}.{}: want {}, have {}",
                        key, drift.key, drift.expected, drift.actual
                    ));
                }
            }
            Err(e) => {
                printer.warning(&format!("{}: error checking drift — {}", key, e));
            }
            _ => {}
        }
    }
    if !has_system_drift {
        printer.success("No system drift");
    }

    if exit_code && (has_file_diff || has_pkg_drift || has_system_drift) {
        cfgd_core::exit::ExitCode::DriftDetected.exit();
    }

    Ok(())
}

/// Print package drift to the printer. Returns `true` when at least one
/// non-Skip action exists (i.e. drift is present).
fn print_package_drift(pkg_actions: &[PackageAction], printer: &Printer) -> bool {
    let pkg_diffs: Vec<&PackageAction> = pkg_actions
        .iter()
        .filter(|a| !matches!(a, PackageAction::Skip { .. }))
        .collect();
    let has_drift = !pkg_diffs.is_empty();
    if pkg_diffs.is_empty() {
        printer.success("No package drift");
    } else {
        for action in &pkg_diffs {
            match action {
                PackageAction::Bootstrap {
                    manager, method, ..
                } => {
                    printer.warning(&format!(
                        "{}: not installed — can bootstrap via {}",
                        manager, method
                    ));
                }
                PackageAction::Install {
                    manager, packages, ..
                } => {
                    printer.warning(&format!("{}: missing — {}", manager, packages.join(", ")));
                }
                PackageAction::Uninstall {
                    manager, packages, ..
                } => {
                    printer.warning(&format!("{}: extra — {}", manager, packages.join(", ")));
                }
                PackageAction::Skip { .. } => {}
            }
        }
    }
    has_drift
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_package_drift_no_drift() {
        let (printer, buf) = Printer::for_test();
        let actions = vec![PackageAction::Skip {
            manager: "brew".into(),
            reason: "up to date".into(),
            origin: "profile".into(),
        }];
        print_package_drift(&actions, &printer);
        let output = buf.lock().unwrap().clone();
        assert!(
            output.contains("No package drift"),
            "all-skip should show no drift, got: {output}"
        );
    }

    #[test]
    fn print_package_drift_missing_packages() {
        let (printer, buf) = Printer::for_test();
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
        print_package_drift(&actions, &printer);
        let output = buf.lock().unwrap().clone();
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
    }
}
