use super::*;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VerifyOutput {
    results: Vec<cfgd_core::reconciler::VerifyResult>,
    pass_count: usize,
    fail_count: usize,
}

fn print_verify_results(results: &[reconciler::VerifyResult], printer: &Printer) -> (usize, usize) {
    let mut pass_count = 0;
    let mut fail_count = 0;

    for result in results {
        if result.matches {
            pass_count += 1;
            printer.success(&format!(
                "{} {} — {}",
                result.resource_type, result.resource_id, result.expected
            ));
        } else {
            fail_count += 1;
            printer.error(&format!(
                "{} {} — want: {}, have: {}",
                result.resource_type, result.resource_id, result.expected, result.actual
            ));
        }
    }

    (pass_count, fail_count)
}

pub(super) fn cmd_verify(
    cli: &Cli,
    printer: &Printer,
    module_filter: Option<&str>,
    exit_code: bool,
) -> anyhow::Result<()> {
    let config_dir = config_dir(cli);
    let state = open_state_store(cli.state_dir.as_deref())?;

    let (resolved, resolved_modules, registry) = if let Some(mod_name) = module_filter {
        let resolved = empty_resolved_profile(mod_name);
        let registry = build_registry();
        let platform = Platform::detect();
        let mgr_map = managers_map(&registry);
        let cache_base = modules::default_module_cache_dir()?;
        let mods = modules::resolve_modules(
            &[mod_name.to_string()],
            &config_dir,
            &cache_base,
            &platform,
            &mgr_map,
            printer,
        )
        .unwrap_or_default();
        (resolved, mods, registry)
    } else {
        let (_cfg, mut resolved) = load_config_and_profile(cli, printer)?;
        packages::resolve_manifest_packages(&mut resolved.merged.packages, &config_dir)?;
        let registry = build_registry_with_profile(&resolved.merged.packages);
        (resolved, Vec::new(), registry)
    };

    let results = reconciler::verify(&resolved, &registry, &state, printer, &resolved_modules)?;

    let pass_count = results.iter().filter(|r| r.matches).count();
    let fail_count = results.iter().filter(|r| !r.matches).count();

    if printer.is_structured() {
        let has_drift = fail_count > 0;
        printer.write_structured(&VerifyOutput {
            results,
            pass_count,
            fail_count,
        });
        if exit_code && has_drift {
            cfgd_core::exit::ExitCode::DriftDetected.exit();
        }
        return Ok(());
    }

    printer.header("Verify");
    printer.newline();

    if results.is_empty() {
        printer.info("No managed resources to verify");
        return Ok(());
    }

    let (pass_count, fail_count) = print_verify_results(&results, printer);

    printer.newline();
    if fail_count == 0 {
        printer.success(&format!(
            "All {} resource(s) match desired state",
            pass_count
        ));
    } else {
        printer.warning(&format!("{} passed, {} failed", pass_count, fail_count));
    }

    if exit_code && fail_count > 0 {
        cfgd_core::exit::ExitCode::DriftDetected.exit();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_verify_results_all_pass() {
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        let results = vec![reconciler::VerifyResult {
            resource_type: "package".into(),
            resource_id: "curl".into(),
            expected: "installed".into(),
            actual: "installed".into(),
            matches: true,
        }];
        let (pass, fail) = print_verify_results(&results, &printer);
        assert_eq!(pass, 1);
        assert_eq!(fail, 0);
    }

    #[test]
    fn print_verify_results_with_failures() {
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        let results = vec![
            reconciler::VerifyResult {
                resource_type: "package".into(),
                resource_id: "curl".into(),
                expected: "installed".into(),
                actual: "installed".into(),
                matches: true,
            },
            reconciler::VerifyResult {
                resource_type: "sysctl".into(),
                resource_id: "net.ipv4.ip_forward".into(),
                expected: "1".into(),
                actual: "0".into(),
                matches: false,
            },
        ];
        let (pass, fail) = print_verify_results(&results, &printer);
        assert_eq!(pass, 1);
        assert_eq!(fail, 1);
    }
}
