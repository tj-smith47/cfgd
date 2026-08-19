use super::*;
use cfgd_core::output::{Doc, Printer, Role};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyOutput {
    pub results: Vec<cfgd_core::reconciler::VerifyResult>,
    pub pass_count: usize,
    pub fail_count: usize,
}

pub fn cmd_verify(
    cli: &Cli,
    printer: &Printer,
    module_filter: Option<&str>,
    exit_code: bool,
) -> anyhow::Result<()> {
    let ctx = RunContext::new(cli, printer);
    let config_dir = ctx.config_dir();
    let state = ctx.state()?;

    let (resolved, resolved_modules, mut registry) = if let Some(mod_name) = module_filter {
        let resolved = empty_resolved_profile(&[mod_name.to_string()], &ctx.active_profile_name());
        let registry = build_registry();
        let platform = Platform::current();
        let mgr_map = registry.manager_map();
        let cache_base = module_cache_dir(cli)?;
        let mods = match modules::resolve_modules(
            &[mod_name.to_string()],
            config_dir,
            &cache_base,
            &[],
            platform,
            &mgr_map,
            printer,
        ) {
            Ok(mods) => mods,
            // "not found" is reserved for a genuinely unknown module name and
            // degrades to the same empty-results "No managed resources to
            // verify" render the rest of this function already produces for
            // it; any other resolution failure (e.g. a dependency cycle among
            // local modules) must surface as the error it is, not read as a
            // miss. This call passes an empty `source_roots`, so
            // `ScriptsNotAllowed` can never originate here — that constraint
            // is enforced only where a source's own module roots are
            // resolved (`resolve_desired_state`).
            Err(e)
                if matches!(
                    &e,
                    cfgd_core::errors::CfgdError::Module(
                        cfgd_core::errors::ModuleError::NotFound { .. }
                    )
                ) =>
            {
                Vec::new()
            }
            Err(e) => return Err(e.into()),
        };
        (resolved, mods, registry)
    } else {
        let (cfg, _profile_name, local_resolved) = ctx.config_and_profile()?;
        // Compose with sources (cache-only — read paths stay offline) and resolve
        // the effective module set through the one shared resolver, so `verify`
        // checks the same source-composed desired state that `apply` writes.
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
        // Taken before the other fields, because a partial move out of
        // `desired` would block the `&mut self` this accessor needs.
        let registry = desired.take_registry(cfg);
        let mut resolved = desired.resolved;
        let mods = desired.modules;
        ctx.resolve_manifest_packages(&mut resolved.merged.packages)?;
        (resolved, mods, registry)
    };
    registry.set_system_config_dir(config_dir);

    // ONE context for both halves of the run: the reconciler's package check
    // and the manager-drift plan below both diff against installed state, and
    // sharing the context is what makes that one enumeration per manager for
    // the whole command instead of one per half.
    let pkg_cx = cfgd_core::providers::PackageContext::new(printer, state);
    let mut results = reconciler::verify(&resolved, &registry, state, &resolved_modules, &pkg_cx)?;
    // The reconciler cannot reach the file manager (crate boundary), so it no
    // longer checks managed files. Fold in content-aware file results here so a
    // file whose bytes drifted out-of-band fails verification and drives
    // `verify --exit-code` to 5. Module-filter runs (empty merged profile) have
    // no managed files, so the profile-file fold is a no-op for them.
    // ONE file manager for both folds: each construction rebuilds the template
    // context and the full secret-provider set, and both halves check the same
    // profile.
    let fm = CfgdFileManager::new(config_dir, &resolved)?;
    results.extend(super::live_drift::file_verify_results(&fm, &resolved)?);
    // Module files are content-aware here (not in the reconciler, which is
    // presence-blind across the crate boundary): a byte-tampered module file
    // fails verification for both the full and `--module` paths.
    results.extend(super::live_drift::module_file_verify_results(
        &fm,
        config_dir,
        &resolved,
        &resolved_modules,
    )?);
    // Managers: the reconciler's own `verify` only walks
    // `available_package_managers`, so a manager the plan would provision or
    // refuse contributes no row there — the same gap `diff` and `status -e`
    // close via `plan_managers`. Fold that half in here too, so `verify -e`
    // cannot report clean on a host `diff`/`status -e` both flag as drifted.
    let cfgd_installed = cfgd_installed_packages(state)?;
    results.extend(super::live_drift::manager_verify_results(
        &resolved,
        &registry,
        &resolved_modules,
        &cfgd_installed,
        &pkg_cx,
    )?);
    let pass_count = results.iter().filter(|r| r.matches).count();
    let fail_count = results.iter().filter(|r| !r.matches).count();
    let has_drift = fail_count > 0;

    let output = VerifyOutput {
        results,
        pass_count,
        fail_count,
    };
    printer.emit(build_verify_doc(&output));

    if exit_code && has_drift {
        cfgd_core::exit::ExitCode::DriftDetected.exit();
    }
    Ok(())
}

/// Pure builder: verify Doc from a collected `VerifyOutput`. Used by the live
/// command and by snapshot tests under `tests/output_snapshots/verify/`.
pub fn build_verify_doc(output: &VerifyOutput) -> Doc {
    let mut doc = Doc::new().heading("Verify");

    if output.results.is_empty() {
        doc = doc.status(Role::Info, "No managed resources to verify");
        return doc.with_data(output.clone());
    }

    doc = doc.section("Resources", |s| {
        output.results.iter().fold(s, |s, r| {
            if r.matches {
                s.status(
                    Role::Ok,
                    format!("{} {} — {}", r.resource_type, r.resource_id, r.expected),
                )
            } else {
                s.status_with(
                    Role::Fail,
                    format!("{} {}", r.resource_type, r.resource_id),
                    |sf| sf.drift(&r.expected, &r.actual),
                )
            }
        })
    });

    doc = if output.fail_count == 0 {
        doc.status(
            Role::Ok,
            format!(
                "All {} {} desired state",
                cfgd_core::pluralize(output.pass_count, "resource"),
                cfgd_core::agreeing_verb(output.pass_count, "match")
            ),
        )
    } else {
        doc.status(
            Role::Warn,
            format!("{} passed, {} failed", output.pass_count, output.fail_count),
        )
    };

    doc.with_data(output.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    use serial_test::serial;

    /// `cfgd verify --module <name>` against a local module set carrying a
    /// dependency cycle must surface the typed `ModuleError::DependencyCycle`
    /// — the real reason resolution failed — rather than degrading to the
    /// empty-results "No managed resources to verify" render `cmd_verify`'s
    /// `Err(e) if matches!(.., NotFound { .. }) => Vec::new()` arm reserves
    /// for a genuinely unknown module name. A prior revision matched on any
    /// `Err(_)` here and silently reported zero resources for every
    /// resolution failure alike, which is exactly what this pins against
    /// regressing. `resolve_modules` is called with an empty `source_roots`
    /// at this call site (see the comment above the match), so a dependency
    /// cycle — not `ScriptsNotAllowed`, which needs a source's own module
    /// roots — is the reachable non-`NotFound` error here.
    #[test]
    #[serial]
    fn cmd_verify_module_dependency_cycle_surfaces_the_real_error() {
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

        let err = cmd_verify(&cli, &printer, Some("cycle-a"), false).unwrap_err();
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

    fn passing_result() -> reconciler::VerifyResult {
        reconciler::VerifyResult {
            resource_type: "package".into(),
            resource_id: "curl".into(),
            expected: "installed".into(),
            actual: "installed".into(),
            matches: true,
        }
    }

    fn failing_result() -> reconciler::VerifyResult {
        reconciler::VerifyResult {
            resource_type: "sysctl".into(),
            resource_id: "net.ipv4.ip_forward".into(),
            expected: "1".into(),
            actual: "0".into(),
            matches: false,
        }
    }

    #[test]
    fn build_verify_doc_renders_passing_resources() {
        let (printer, cap) = Printer::for_test_doc();
        let output = VerifyOutput {
            results: vec![passing_result()],
            pass_count: 1,
            fail_count: 0,
        };
        printer.emit(build_verify_doc(&output));
        drop(printer);
        let human = cap.human();
        assert!(
            human.contains("package"),
            "expected resource_type, got: {human}"
        );
        assert!(human.contains("curl"), "expected resource_id, got: {human}");
        assert!(
            human.contains("installed"),
            "expected expected-value, got: {human}"
        );
        assert!(
            human.contains("All 1 resource matches desired state"),
            "expected summary line, got: {human}"
        );
    }

    #[test]
    fn build_verify_doc_renders_failures_with_actual() {
        let (printer, cap) = Printer::for_test_doc();
        let output = VerifyOutput {
            results: vec![failing_result()],
            pass_count: 0,
            fail_count: 1,
        };
        printer.emit(build_verify_doc(&output));
        drop(printer);
        let human = cap.human();
        assert!(
            human.contains("sysctl"),
            "expected resource_type, got: {human}"
        );
        assert!(
            human.contains("net.ipv4.ip_forward"),
            "expected resource_id, got: {human}"
        );
        assert!(
            human.contains("want: 1"),
            "expected want-line, got: {human}"
        );
        assert!(
            human.contains("have: 0"),
            "expected have-line, got: {human}"
        );
        assert!(
            human.contains("0 passed, 1 failed"),
            "expected summary line, got: {human}"
        );
    }
}
