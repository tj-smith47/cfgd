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
        let pkg_cx = ctx.package_context()?;
        let mods = match modules::resolve_modules(
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
    // One spinner across all four passes, renamed per pass: they run back to
    // back with no output of their own, and a package enumeration inside the
    // first can take seconds.
    let mut results = printer.narrate("Verifying: resources", |sp| -> anyhow::Result<_> {
        // A `--module` run passes `machine_surfaces: false`: its composition
        // is module-only config, and the system/env halves diff that against
        // machine-wide surfaces — a claim about the machine no single module
        // can vouch for, so a scoped run neither computes, renders, judges
        // nor records it.
        let mut results = reconciler::verify(
            &resolved,
            &registry,
            state,
            &resolved_modules,
            &pkg_cx,
            module_filter.is_none(),
        )?;
        // The reconciler cannot reach the file manager (crate boundary), so it no
        // longer checks managed files. Fold in content-aware file results here so a
        // file whose bytes drifted out-of-band fails verification and drives
        // `verify --exit-code` to 5. Module-filter runs (empty merged profile) have
        // no managed files, so the profile-file fold is a no-op for them.
        // ONE file manager for both folds: each construction rebuilds the template
        // context and the full secret-provider set, and both halves check the same
        // profile.
        sp.set_message("Verifying: profile files");
        let fm = CfgdFileManager::new(config_dir, &resolved)?;
        results.extend(super::live_drift::file_verify_results(
            &fm,
            config_dir,
            &resolved,
            &resolved_modules,
            registry.default_file_strategy,
            state,
        )?);
        // Module files are content-aware here (not in the reconciler, which is
        // presence-blind across the crate boundary): a byte-tampered module file
        // fails verification for both the full and `--module` paths.
        sp.set_message("Verifying: module files");
        results.extend(super::live_drift::module_file_verify_results(
            &fm,
            config_dir,
            &resolved,
            &resolved_modules,
            registry.default_file_strategy,
            state,
        )?);
        // Managers: the reconciler's own `verify` only walks
        // `available_package_managers`, so a manager the plan would provision or
        // refuse contributes no row there — the same gap `diff` and `status --scan`
        // close via `plan_managers`. Fold that half in here too, so `verify -e`
        // cannot report clean on a host `diff`/`status --scan` both flag as drifted.
        // FULL runs only: manager provisioning is a machine-wide surface, the
        // same scope rule as the system/env halves — and the same shape as
        // `diff --module`/`status <mod> --scan`, which plan no managers.
        if module_filter.is_none() {
            sp.set_message("Verifying: package managers");
            let cfgd_installed = cfgd_installed_packages(state)?;
            results.extend(super::live_drift::manager_verify_results(
                &resolved,
                &registry,
                &resolved_modules,
                &cfgd_installed,
                &pkg_cx,
            )?);
        }
        Ok(results)
    })?;
    // `reconciler::verify` is pure compute — this seam is where its results
    // become recorded rows, from the producer literals, BEFORE the display
    // recompute below rewrites env rows to their declared values
    // (`live_drift`'s module doc). The fleet-wide run is a FULL-machine
    // check: every finding lands as a row and every recorded row it
    // re-checked and did not re-find resolves as healed. Which system
    // namespaces it re-checked is read off the results themselves: a
    // configurator whose diff produced any row (clean or drifted) vouches
    // for its `<configurator>.` prefix, and one that errored or never ran
    // contributes no system row at all, so its recorded rows stand. A
    // `--module` run computes nothing but its own module's files and
    // packages, so it records and resolves exactly what it checked.
    if module_filter.is_none() {
        let mut evaluated_system: Vec<String> = results
            .iter()
            .filter(|r| r.resource_type == "system")
            .filter_map(|r| r.resource_id.split('.').next())
            .map(str::to_string)
            .collect();
        evaluated_system.sort_unstable();
        evaluated_system.dedup();
        super::live_drift::record_full_scan_findings(
            state,
            results.iter().filter(|r| !r.matches),
            &evaluated_system,
        );
    } else {
        // A scoped run computes ONLY its own module's files and packages —
        // the machine-wide halves are gated off above — so everything in
        // `results` is the scope: what it renders, what drives its exit
        // code, and what it records and resolves are one set.
        let checked: Vec<(String, String)> = results
            .iter()
            .map(|r| (r.resource_type.clone(), r.resource_id.clone()))
            .collect();
        super::live_drift::record_scoped_scan_findings(
            state,
            &checked,
            results.iter().filter(|r| !r.matches),
        );
    }
    // The recording above persisted the opaque `current`/`missing or
    // changed` markers for every env-var/alias row (the declared value must
    // never reach `drift_events`) — this `results` vec is now the DISPLAY
    // copy, rendered below into `build_verify_doc`'s human/`-o json` output.
    // Recomputing here is exactly `diff`'s "opaque markers carry neither
    // real value" rule applied to `verify`'s own render.
    let merged_env_items = reconciler::MergedEnvItems::new(
        &resolved.merged.env,
        &resolved.merged.aliases,
        &resolved.merged.entry_owners,
        &resolved_modules,
        &reconciler::recorded_manager_path_dirs(state, &resolved.merged, &resolved_modules),
    );
    for r in &mut results {
        if let Some((expected, actual)) =
            merged_env_items.display_values(&r.resource_type, &r.resource_id)
        {
            r.expected = expected;
            r.actual = actual;
        }
    }
    // A FLEET-wide verify just checked the machine itself, whatever it found —
    // the recorded-state `status` header dates its display from here, and a
    // scan that finds nothing is exactly the one a clean host has no other
    // record of. A `--module` run checked one module's files and packages and
    // must not re-date the whole dashboard, the same rule `diff --module` and
    // `status --scan --module` follow.
    if module_filter.is_none() {
        state.record_scan();
    }
    let pass_count = results.iter().filter(|r| r.matches).count();
    let fail_count = results.iter().filter(|r| !r.matches).count();
    let has_drift = fail_count > 0;

    let output = VerifyOutput {
        results,
        pass_count,
        fail_count,
    };
    printer.emit(build_verify_doc(&output, module_filter));

    if exit_code && has_drift {
        cfgd_core::exit::ExitCode::DriftDetected.exit();
    }
    Ok(())
}

/// Pure builder: verify Doc from a collected `VerifyOutput`. Used by the live
/// command and by snapshot tests under `tests/output_snapshots/verify/`.
///
/// `module` is the `--module` filter the run carried, so a report that failed
/// closes on a next step scoped the way the report was.
pub fn build_verify_doc(output: &VerifyOutput, module: Option<&str>) -> Doc {
    let mut doc = Doc::new().heading("Verify");

    if output.results.is_empty() {
        doc = doc.status(Role::Info, "No managed resources to verify");
        return doc.with_data(output.clone());
    }

    // No env-file-freshness suppression here, unlike the two drift REPORTS:
    // `verify` is a ledger whose closing line counts its own rows, so a row
    // hidden from the list is a row the tally still charges for.
    doc = doc.section("Resources", |s| {
        output.results.iter().fold(s, |s, r| {
            let subject = cfgd_core::output::drift_item_subject(&r.resource_type, &r.resource_id);
            let (expected, actual) =
                cfgd_core::output::drift_operands(&r.resource_type, &r.expected, &r.actual);
            if r.matches {
                s.status_with(Role::Ok, subject, |sf| sf.detail(expected))
            } else {
                s.status_with(Role::Fail, subject, |sf| sf.drift(&expected, &actual))
            }
        })
    });

    doc = if output.fail_count == 0 {
        doc.status(
            // verdict-row-ok: a match verdict, not an act cfgd performed
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
        .hint(super::heal_drift_hint(module))
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

    /// The machine-wide "last scan" stamp is written by a fleet-wide verify
    /// and by nothing narrower.
    ///
    /// That stamp is the sole input to the recorded `status` header's age line
    /// and to its `--scan` hint, so a run that checked ONE module's files and
    /// packages must leave it alone — the rule `diff --module` and
    /// `status --scan --module` already follow. Both arms run against the same
    /// fixture and the same state store, so the only thing separating them is
    /// the filter.
    #[test]
    #[serial]
    fn cmd_verify_stamps_the_machine_scan_only_when_it_checked_the_machine() {
        use crate::cli::helpers::tests::{make_cli, quiet_printer};

        let tmp = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp.path());
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
        let mod_dir = tmp.path().join("modules").join("test-mod");
        std::fs::create_dir_all(&mod_dir).unwrap();
        std::fs::write(
            mod_dir.join("module.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-mod\nspec: {}\n",
        )
        .unwrap();

        let state_dir = tmp.path().join("state");
        let mut cli = make_cli(config_path);
        cli.state_dir = Some(state_dir.clone());
        cli.cache_dir = Some(tmp.path().join("cache"));
        let printer = quiet_printer();

        cmd_verify(&cli, &printer, Some("test-mod"), false).unwrap();
        let stamp_after_module = open_state_store(Some(&state_dir), cfgd_core::Scope::User)
            .unwrap()
            .last_scan_at()
            .unwrap();
        assert_eq!(
            stamp_after_module, None,
            "one module's verify re-dated the whole dashboard"
        );

        cmd_verify(&cli, &printer, None, false).unwrap();
        let stamp_after_fleet = open_state_store(Some(&state_dir), cfgd_core::Scope::User)
            .unwrap()
            .last_scan_at()
            .unwrap();
        assert!(
            stamp_after_fleet.is_some(),
            "a fleet-wide verify checked the machine and must date the dashboard"
        );
    }

    /// The CLI recording seam persists the opaque `current`/`missing or
    /// changed` markers for a drifted env-var/alias row — correctly, since
    /// the declared value must stay out of `drift_events` — but
    /// `cmd_verify`'s own DISPLAY of that same `results` vec has to recompute
    /// the real declared line, or `cfgd verify`'s human and `-o json` renders
    /// show the storage marker instead of the value. `status --scan` is the
    /// sibling display consumer of the identical per-item `VerifyResult`s,
    /// and needs the same `env_item_display_values` recompute.
    #[test]
    #[serial]
    fn cmd_verify_shows_the_declared_env_value_not_the_opaque_marker() {
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
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  envScope: Interactive\n  env:\n    - name: EDITOR\n      value: vim\n",
        )
        .unwrap();

        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        // Header only, no `EDITOR` line — the per-item check reports the
        // declared var as drifted.
        std::fs::write(
            cfgd_core::reconciler::primary_env_file(tmp_home.path()),
            "# managed by cfgd \u{2014} do not edit\n",
        )
        .unwrap();
        // The declared line's dialect is platform-dependent, so the expected
        // needle is derived from `env_item_declared_line` (production's own
        // per-item renderer for the running platform) rather than a
        // hardcoded POSIX literal.
        let declared_env = vec![cfgd_core::config::EnvVar {
            name: "EDITOR".to_string(),
            value: "vim".to_string(),
            platforms: vec![],
        }];
        // The owners the profile-layer merge records for this profile: the
        // generated line names its layer, so a needle rendered with no owner
        // is a line the file never holds.
        let declared_owners = {
            let mut o = cfgd_core::config::EntryOwners::default();
            o.claim("profile:default", &declared_env, &[]);
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

        let state_dir = tmp.path().join("state");
        let mut cli = make_cli(config_path);
        cli.state_dir = Some(state_dir);
        cli.cache_dir = Some(tmp.path().join("cache"));

        let (printer, cap) = Printer::for_test_doc();
        cmd_verify(&cli, &printer, None, false).unwrap();
        drop(printer);

        let human = cap.human();
        let editor_line = human
            .lines()
            .find(|l| l.contains("env: EDITOR"))
            .unwrap_or_else(|| panic!("expected an env EDITOR line, got: {human}"));
        assert!(
            editor_line.contains(&declared_line),
            "the declared line must be visible, got: {editor_line}"
        );
        assert!(
            !editor_line.contains("current"),
            "must not regress to the opaque marker, got: {editor_line}"
        );

        let json = cap.json().expect("verify emits a data payload");
        let results = json["results"].as_array().expect("results array");
        let editor_row = results
            .iter()
            .find(|r| r["resourceType"] == "env-var" && r["resourceId"] == "EDITOR")
            .unwrap_or_else(|| panic!("expected an EDITOR result row: {json}"));
        assert_eq!(
            editor_row["expected"],
            serde_json::json!(declared_line),
            "the -o json payload must carry the declared line: {editor_row}"
        );
    }

    /// A fleet-wide `cfgd verify` is a FULL-machine live check: every finding
    /// lands as a `drift_events` row (in the producer's own literals — a
    /// declared env value never reaches the store), every recorded row it
    /// re-checked and did not re-find resolves as healed, and every recorded
    /// row it CANNOT re-find stands: a class it never evaluates (`secret`,
    /// `script`), a daemon-spelled id (`system` with `:`, a comma-batched
    /// `package`, a `ModuleAction`'s bare module name, a
    /// `PackageAction::Skip`'s bare manager name), and a `system` row of a
    /// configurator this run never probed. The kept rows are their own
    /// writer's to resolve; a verify that cleared them would erase findings
    /// nothing re-checked.
    #[test]
    #[serial_test::serial]
    fn a_full_verify_records_its_findings_and_keeps_rows_it_cannot_refind() {
        use crate::cli::helpers::tests::{make_cli, quiet_printer};

        let tmp = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp.path());
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n",
        )
        .unwrap();
        let files_dir = tmp.path().join("files");
        std::fs::create_dir_all(&files_dir).unwrap();
        std::fs::write(files_dir.join("managed.txt"), "declared content\n").unwrap();
        let absent_target = tmp.path().join("absent.txt");
        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(
            profiles_dir.join("default.yaml"),
            format!(
                "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  files:\n    managed:\n      - source: files/managed.txt\n        target: {}\n        strategy: Copy\n  env:\n    - name: FIXTURE_TOKEN\n      value: sk-fixture-secret\n",
                cfgd_core::to_posix_string(&absent_target)
            ),
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("modules")).unwrap();

        let state_dir = tmp.path().join("state");
        let mut cli = make_cli(config_path);
        cli.state_dir = Some(state_dir.clone());
        cli.cache_dir = Some(tmp.path().join("cache"));

        // Rows a full verify cannot re-find (they must stand), plus one stale
        // row of a class it DOES re-check (it must resolve). The last two are
        // the daemon's bare action spellings: a `ModuleAction`'s module name
        // (no `/`) and a `PackageAction::Skip`'s manager name (no `:`).
        let kept: [(&str, &str); 7] = [
            ("secret", "op://vault/item"),
            ("script", "echo hi"),
            ("system", "sysctl:vm.swappiness"),
            ("package", "brew:jq,ripgrep"),
            ("system", "ghostcfg.some.key"),
            ("module", "nvim"),
            ("package", "brew"),
        ];
        {
            let store = open_state_store(Some(&state_dir), cfgd_core::Scope::User).unwrap();
            for (rtype, rid) in kept {
                store
                    .record_drift(rtype, rid, Some("x"), Some("y"), "daemon")
                    .unwrap();
            }
            store
                .record_drift(
                    "file",
                    &cfgd_core::to_posix_string(tmp.path().join("stale.txt")),
                    Some("current"),
                    Some("missing"),
                    "local",
                )
                .unwrap();
        }

        let printer = quiet_printer();
        cmd_verify(&cli, &printer, None, false).unwrap();

        let store = open_state_store(Some(&state_dir), cfgd_core::Scope::User).unwrap();
        let rows = store.unresolved_drift().unwrap();
        assert!(
            rows.iter().any(|e| e.resource_type == "file"
                && e.resource_id == cfgd_core::to_posix_string(&absent_target)),
            "the missing managed file must be recorded, got: {rows:?}"
        );
        for (rtype, rid) in kept {
            assert!(
                rows.iter()
                    .any(|e| e.resource_type == rtype && e.resource_id == rid),
                "a row this check cannot re-find must stand: {rtype}/{rid}, got: {rows:?}"
            );
        }
        assert!(
            !rows.iter().any(|e| e.resource_id.contains("stale.txt")),
            "a file row the check re-checked and did not re-find must resolve, got: {rows:?}"
        );
        for e in &rows {
            for op in [&e.expected, &e.actual] {
                assert!(
                    !op.as_deref().unwrap_or("").contains("sk-fixture-secret"),
                    "a declared env value must never reach the store, got: {e:?}"
                );
            }
        }
        assert!(
            store.last_scan_at().unwrap().is_some(),
            "a fleet-wide verify checked the machine and must date the dashboard"
        );
    }

    /// The `system` arm of the keep-set is derived, not assumed: a full
    /// verify vouches only for the configurators its own results carry, so a
    /// stale row under a namespace it evaluated resolves while one under a
    /// configurator nothing probed stands. Linux-only because the fixture's
    /// configurator is `gsettings`, which the registry registers only on
    /// Linux; the seam makes it available and answers its bulk read with the
    /// declared value, so the clean probe still counts as evaluated.
    #[cfg(target_os = "linux")]
    #[test]
    #[serial_test::serial]
    fn a_full_verify_resolves_stale_rows_only_under_configurators_it_evaluated() {
        use crate::cli::helpers::tests::{make_cli, quiet_printer};

        let _shim = cfgd_core::test_helpers::ToolShim::install(
            "CFGD_GSETTINGS_BIN",
            0,
            "org.gnome.cfgd key 'declared'\n",
            "",
        );
        let tmp = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp.path());
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
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  system:\n    gsettings:\n      org.gnome.cfgd:\n        key: declared\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("modules")).unwrap();

        let state_dir = tmp.path().join("state");
        let mut cli = make_cli(config_path);
        cli.state_dir = Some(state_dir.clone());
        cli.cache_dir = Some(tmp.path().join("cache"));

        {
            let store = open_state_store(Some(&state_dir), cfgd_core::Scope::User).unwrap();
            store
                .record_drift(
                    "system",
                    "gsettings.stale.key",
                    Some("x"),
                    Some("y"),
                    "daemon",
                )
                .unwrap();
            store
                .record_drift(
                    "system",
                    "ghostcfg.other.key",
                    Some("x"),
                    Some("y"),
                    "daemon",
                )
                .unwrap();
        }

        let printer = quiet_printer();
        cmd_verify(&cli, &printer, None, false).unwrap();

        let store = open_state_store(Some(&state_dir), cfgd_core::Scope::User).unwrap();
        let rows = store.unresolved_drift().unwrap();
        assert!(
            !rows.iter().any(|e| e.resource_id == "gsettings.stale.key"),
            "a stale row under an evaluated configurator must resolve, got: {rows:?}"
        );
        assert!(
            rows.iter().any(|e| e.resource_id == "ghostcfg.other.key"),
            "a row under a configurator nothing probed must stand, got: {rows:?}"
        );
    }

    /// A `--module` verify is evidence about ONE module: it computes,
    /// renders, judges and records only its own module's files and packages.
    /// The machine-wide env/system/manager comparison its empty-profile
    /// composition would make (module-only config against machine-wide
    /// surfaces — factually wrong as a machine claim) reaches neither the
    /// store NOR the rendered report and exit verdict, and foreign rows and
    /// the machine-wide stamp stay untouched.
    #[test]
    #[serial_test::serial]
    fn a_module_scoped_verify_records_only_its_own_modules_rows_and_no_stamp() {
        use crate::cli::helpers::tests::make_cli;

        let tmp = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp.path());
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
        let mod_dir = tmp.path().join("modules").join("test-mod");
        std::fs::create_dir_all(mod_dir.join("files")).unwrap();
        std::fs::write(mod_dir.join("files").join("app.conf"), "app config\n").unwrap();
        std::fs::write(mod_dir.join("files").join("ok.conf"), "converged\n").unwrap();
        let missing_target = tmp.path().join("mod-target.txt");
        let healed_target = tmp.path().join("mod-healed.txt");
        std::fs::write(&healed_target, "converged\n").unwrap();
        std::fs::write(
            mod_dir.join("module.yaml"),
            format!(
                "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-mod\nspec:\n  env:\n    - name: FOO\n      value: sk-module-secret\n  files:\n    - source: files/app.conf\n      target: {}\n    - source: files/ok.conf\n      target: {}\n",
                cfgd_core::to_posix_string(&missing_target),
                cfgd_core::to_posix_string(&healed_target)
            ),
        )
        .unwrap();

        let state_dir = tmp.path().join("state");
        let mut cli = make_cli(config_path);
        cli.state_dir = Some(state_dir.clone());
        cli.cache_dir = Some(tmp.path().join("cache"));

        let healed_id = super::super::live_drift::module_file_resource_id(
            "test-mod",
            &cfgd_core::to_posix_string(&healed_target),
        );
        {
            let store = open_state_store(Some(&state_dir), cfgd_core::Scope::User).unwrap();
            // Foreign rows a scoped run may not touch, and one own-scope row
            // the run re-checks and no longer finds drifted.
            for (rtype, rid) in [
                ("module", "other-mod/etc/other.conf"),
                ("file", "/etc/hosts"),
                ("module", healed_id.as_str()),
            ] {
                store
                    .record_drift(rtype, rid, Some("x"), Some("y"), "daemon")
                    .unwrap();
            }
        }

        let (printer, buf) = Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
        cmd_verify(&cli, &printer, Some("test-mod"), false).unwrap();
        drop(printer);
        // The DISPLAY half of the scope rule: the rendered report carries the
        // module's own finding and none of the machine-wide env/rc rows the
        // full run would show — `fail_count` and the exit verdict read the
        // same vec, so a leak here is also a wrongful exit 5.
        let out = cfgd_core::test_helpers::captured_text(&buf);
        assert!(
            out.contains("mod-target.txt"),
            "the scoped report must render its own module's finding, got: {out}"
        );
        for machine_row in [
            ".cfgd.env",
            ".bashrc",
            ".zshenv",
            ".profile",
            "environment.d",
        ] {
            assert!(
                !out.contains(machine_row),
                "a scoped verify may not render the machine-wide env comparison \
                 ({machine_row}), got: {out}"
            );
        }

        let store = open_state_store(Some(&state_dir), cfgd_core::Scope::User).unwrap();
        let rows = store.unresolved_drift().unwrap();
        let missing_id = super::super::live_drift::module_file_resource_id(
            "test-mod",
            &cfgd_core::to_posix_string(&missing_target),
        );
        assert!(
            rows.iter()
                .any(|e| e.resource_type == "module" && e.resource_id == missing_id),
            "the scoped run must record its own module's finding, got: {rows:?}"
        );
        assert!(
            !rows.iter().any(|e| e.resource_id == healed_id),
            "an own-scope row the run re-checked clean must resolve, got: {rows:?}"
        );
        for (rtype, rid) in [
            ("module", "other-mod/etc/other.conf"),
            ("file", "/etc/hosts"),
        ] {
            assert!(
                rows.iter()
                    .any(|e| e.resource_type == rtype && e.resource_id == rid),
                "a foreign row must stand: {rtype}/{rid}, got: {rows:?}"
            );
        }
        // Exact population, not a filter: the store holds the two foreign rows
        // plus the scoped run's own finding and NOTHING else (the machine-wide
        // env/system halves included) — a filter shaped around today's row
        // types goes vacuous the moment a new producer mints a new shape.
        let mut standing: Vec<(&str, &str)> = rows
            .iter()
            .map(|e| (e.resource_type.as_str(), e.resource_id.as_str()))
            .collect();
        standing.sort_unstable();
        let mut expected_rows = vec![
            ("file", "/etc/hosts"),
            ("module", "other-mod/etc/other.conf"),
            ("module", missing_id.as_str()),
        ];
        expected_rows.sort_unstable();
        assert_eq!(
            standing, expected_rows,
            "a scoped verify writes no row outside its module's scope"
        );
        for e in &rows {
            for op in [&e.expected, &e.actual] {
                assert!(
                    !op.as_deref().unwrap_or("").contains("sk-module-secret"),
                    "a declared env value must never reach the store, got: {e:?}"
                );
            }
        }
        assert_eq!(
            store.last_scan_at().unwrap(),
            None,
            "one module's verify re-dated the whole dashboard"
        );
    }

    fn passing_result() -> reconciler::VerifyResult {
        reconciler::VerifyResult {
            resource_type: "package".into(),
            resource_id: "curl".into(),
            expected: "installed".into(),
            actual: "installed".into(),
            matches: true,
            unmanaged: false,
        }
    }

    fn failing_result() -> reconciler::VerifyResult {
        reconciler::VerifyResult {
            resource_type: "sysctl".into(),
            resource_id: "net.ipv4.ip_forward".into(),
            expected: "1".into(),
            actual: "0".into(),
            matches: false,
            unmanaged: false,
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
        printer.emit(build_verify_doc(&output, None));
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
        printer.emit(build_verify_doc(&output, None));
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
