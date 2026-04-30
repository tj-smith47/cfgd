use super::*;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VerifyOutput {
    results: Vec<cfgd_core::reconciler::VerifyResult>,
    pass_count: usize,
    fail_count: usize,
}

fn print_verify_results(results: &[reconciler::VerifyResult], printer: &Printer) {
    for result in results {
        if result.matches {
            printer.success(&format!(
                "{} {} — {}",
                result.resource_type, result.resource_id, result.expected
            ));
        } else {
            printer.error(&format!(
                "{} {} — want: {}, have: {}",
                result.resource_type, result.resource_id, result.expected, result.actual
            ));
        }
    }
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

    print_verify_results(&results, printer);

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
    fn print_verify_results_renders_passing_resources() {
        let (printer, buf) = Printer::for_test();
        let results = vec![reconciler::VerifyResult {
            resource_type: "package".into(),
            resource_id: "curl".into(),
            expected: "installed".into(),
            actual: "installed".into(),
            matches: true,
        }];
        print_verify_results(&results, &printer);
        let out = buf.lock().unwrap();
        assert!(
            out.contains("package"),
            "expected resource_type, got: {out}"
        );
        assert!(out.contains("curl"), "expected resource_id, got: {out}");
        assert!(
            out.contains("installed"),
            "expected expected-value, got: {out}"
        );
    }

    #[test]
    fn print_verify_results_renders_failures_with_actual() {
        let (printer, buf) = Printer::for_test();
        let results = vec![reconciler::VerifyResult {
            resource_type: "sysctl".into(),
            resource_id: "net.ipv4.ip_forward".into(),
            expected: "1".into(),
            actual: "0".into(),
            matches: false,
        }];
        print_verify_results(&results, &printer);
        let out = buf.lock().unwrap();
        assert!(out.contains("sysctl"), "expected resource_type, got: {out}");
        assert!(
            out.contains("net.ipv4.ip_forward"),
            "expected resource_id, got: {out}"
        );
        assert!(out.contains("want: 1"), "expected want-line, got: {out}");
        assert!(out.contains("have: 0"), "expected have-line, got: {out}");
    }
}
