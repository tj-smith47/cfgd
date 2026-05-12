use super::*;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusOutput {
    last_apply: Option<cfgd_core::state::ApplyRecord>,
    drift: Vec<cfgd_core::state::DriftEvent>,
    sources: Vec<cfgd_core::state::ConfigSourceRecord>,
    pending_decisions: Vec<cfgd_core::state::PendingDecision>,
    modules: Vec<ModuleStatusEntry>,
    managed_resources: Vec<cfgd_core::state::ManagedResource>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ModuleStatusEntry {
    name: String,
    packages: usize,
    files: usize,
    status: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ModuleStatus {
    name: String,
    packages: usize,
    files: usize,
    depends: Vec<String>,
    status: String,
    last_applied: Option<String>,
}

pub(super) fn cmd_status(
    cli: &Cli,
    printer: &Printer,
    module_filter: Option<&str>,
    exit_code: bool,
) -> anyhow::Result<()> {
    if let Some(mod_name) = module_filter {
        return cmd_status_module(cli, printer, mod_name);
    }

    let (cfg, resolved) = load_config_and_profile(cli, printer)?;
    let state = open_state_store(cli.state_dir.as_deref())?;

    let last_apply = state.last_apply()?;
    let drift_events = state.unresolved_drift()?;
    let source_records = if !cfg.spec.sources.is_empty() {
        state.config_sources()?
    } else {
        vec![]
    };
    let pending = state.pending_decisions()?;
    let resources = state.managed_resources()?;

    // Build module status entries
    let config_dir = config_dir(cli);
    // default_module_cache_dir() can fail only if $HOME is unset; fall back to
    // empty PathBuf so the status display degrades gracefully instead of erroring.
    let cache_base = modules::default_module_cache_dir().unwrap_or_default();
    // load_all_modules failure (e.g., malformed module YAML) should not abort a
    // read-only status query; degrade to an empty map so the rest still renders.
    let all_modules =
        modules::load_all_modules(&config_dir, &cache_base, printer).unwrap_or_default();
    let state_map = module_state_map(&state);
    let module_entries: Vec<ModuleStatusEntry> = resolved
        .merged
        .modules
        .iter()
        .map(|mod_ref| {
            let mod_name = modules::resolve_profile_module_name(mod_ref);
            let (pkg_count, file_count) = all_modules
                .get(mod_name)
                .map(|m| (m.spec.packages.len(), m.spec.files.len()))
                .unwrap_or((0, 0));
            let status = state_map
                .get(mod_name)
                .map(|s| s.status.clone())
                .unwrap_or_else(|| "not applied".into());
            ModuleStatusEntry {
                name: mod_ref.clone(),
                packages: pkg_count,
                files: file_count,
                status,
            }
        })
        .collect();

    let has_drift = !drift_events.is_empty();

    if printer.is_structured() {
        printer.write_structured(&StatusOutput {
            last_apply,
            drift: drift_events,
            sources: source_records,
            pending_decisions: pending,
            modules: module_entries,
            managed_resources: resources,
        });
        if exit_code && has_drift {
            cfgd_core::exit::ExitCode::DriftDetected.exit();
        }
        return Ok(());
    }

    printer.header("Status");
    printer.newline();

    // Last apply
    if let Some(last) = &last_apply {
        printer.subheader("Last Apply");
        printer.key_value("Time", &last.timestamp);
        printer.key_value("Profile", &last.profile);
        printer.key_value(
            "Status",
            match last.status {
                cfgd_core::state::ApplyStatus::Success => "success",
                cfgd_core::state::ApplyStatus::Partial => "partial",
                cfgd_core::state::ApplyStatus::Failed => "failed",
                cfgd_core::state::ApplyStatus::InProgress => "in_progress",
            },
        );
        if let Some(ref summary) = last.summary {
            printer.key_value("Summary", summary);
        }
    } else {
        printer.info("No applies recorded yet");
    }

    // Drift summary
    printer.newline();
    printer.subheader("Drift");
    if drift_events.is_empty() {
        printer.success("No drift detected");
    } else {
        for event in &drift_events {
            let source_info = if event.source != "local" {
                format!(" [{}]", event.source)
            } else {
                String::new()
            };
            printer.warning(&format!(
                "{} {} — want: {}, have: {}{}",
                event.resource_type,
                event.resource_id,
                event.expected.as_deref().unwrap_or("?"),
                event.actual.as_deref().unwrap_or("?"),
                source_info,
            ));
        }
    }

    // Config sources
    if !cfg.spec.sources.is_empty() {
        printer.newline();
        printer.subheader("Config Sources");
        if source_records.is_empty() {
            for source in &cfg.spec.sources {
                printer.key_value(&source.name, "not yet fetched");
            }
        } else {
            let rows: Vec<Vec<String>> = source_records
                .iter()
                .map(|s| {
                    vec![
                        s.name.clone(),
                        s.status.clone(),
                        s.source_version.clone().unwrap_or_else(|| "-".into()),
                        s.last_fetched.clone().unwrap_or_else(|| "never".into()),
                    ]
                })
                .collect();
            printer.table(&["Source", "Status", "Version", "Last Fetched"], &rows);
        }
    }

    // Pending decisions
    if !pending.is_empty() {
        printer.newline();
        printer.subheader("Pending Decisions");
        display_pending_decisions(printer, &pending);
    }

    // Modules
    if !resolved.merged.modules.is_empty() {
        printer.newline();
        printer.subheader("Modules");

        for mod_ref in &resolved.merged.modules {
            let mod_name = modules::resolve_profile_module_name(mod_ref);
            let (pkg_count, file_count) = all_modules
                .get(mod_name)
                .map(|m| (m.spec.packages.len(), m.spec.files.len()))
                .unwrap_or((0, 0));

            let summary = format!("{} pkgs, {} files", pkg_count, file_count);
            if let Some(state_rec) = state_map.get(mod_name) {
                if state_rec.status == "installed" {
                    printer.success(&format!("{}: {}, {}", mod_ref, summary, state_rec.status));
                } else {
                    printer.warning(&format!("{}: {}, {}", mod_ref, summary, state_rec.status));
                }
            } else {
                printer.info(&format!("{}: {}, not yet applied", mod_ref, summary));
            }
        }
    }

    // Managed resources
    if !resources.is_empty() {
        printer.newline();
        printer.subheader("Managed Resources");
        printer.table(
            &["Type", "Resource", "Source"],
            &resources
                .iter()
                .map(|r| {
                    vec![
                        r.resource_type.clone(),
                        r.resource_id.clone(),
                        r.source.clone(),
                    ]
                })
                .collect::<Vec<_>>(),
        );
    }

    if exit_code && has_drift {
        cfgd_core::exit::ExitCode::DriftDetected.exit();
    }

    Ok(())
}

pub(super) fn cmd_status_module(
    cli: &Cli,
    printer: &Printer,
    mod_name: &str,
) -> anyhow::Result<()> {
    let config_dir = config_dir(cli);
    // Propagate (vs. unwrap_or_default in cmd_status): the module-scoped path
    // queries a single named module, so a missing cache dir means we cannot
    // answer the user's specific question and should error rather than silently
    // claim "module not found".
    let cache_base = modules::default_module_cache_dir()?;
    let all_modules = modules::load_all_modules(&config_dir, &cache_base, printer)?;

    let module = match all_modules.get(mod_name) {
        Some(m) => m,
        None => {
            // Module not found — show empty status gracefully
            if printer.is_structured() {
                printer.write_structured(&ModuleStatus {
                    name: mod_name.to_string(),
                    packages: 0,
                    files: 0,
                    depends: vec![],
                    status: "not found".into(),
                    last_applied: None,
                });
            } else {
                printer.header(&format!("Status: {}", mod_name));
                printer.info(&format!("Module '{}' not found", mod_name));
            }
            return Ok(());
        }
    };

    let state = open_state_store(cli.state_dir.as_deref())?;
    let state_rec = state.module_state_by_name(mod_name)?;

    if printer.is_structured() {
        let status = state_rec
            .as_ref()
            .map(|s| s.status.clone())
            .unwrap_or_else(|| "not applied".into());
        let last_applied = state_rec.as_ref().map(|s| s.installed_at.clone());
        printer.write_structured(&ModuleStatus {
            name: mod_name.to_string(),
            packages: module.spec.packages.len(),
            files: module.spec.files.len(),
            depends: module.spec.depends.clone(),
            status,
            last_applied,
        });
        return Ok(());
    }

    printer.header(&format!("Status: {}", mod_name));

    // Module info
    printer.key_value("Packages", &module.spec.packages.len().to_string());
    printer.key_value("Files", &module.spec.files.len().to_string());
    if !module.spec.depends.is_empty() {
        printer.key_value("Dependencies", &module.spec.depends.join(", "));
    }

    // State from DB
    if let Some(rec) = &state_rec {
        printer.key_value("Status", &rec.status);
        printer.key_value("Last applied", &rec.installed_at);
        printer.key_value("Packages hash", &rec.packages_hash);
        printer.key_value("Files hash", &rec.files_hash);
    } else {
        printer.key_value("Status", "not applied");
    }

    // Show deployed files from manifest
    let deployed_files = state.module_deployed_files(mod_name)?;
    if !deployed_files.is_empty() {
        printer.newline();
        printer.subheader("Deployed Files");
        for f in &deployed_files {
            let exists = std::path::Path::new(&f.file_path).exists();
            if exists {
                printer.success(&f.file_path);
            } else {
                printer.error(&format!("{} (missing)", f.file_path));
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cfgd_core::output::OutputFormat;
    use cfgd_core::state::ApplyStatus;

    // Minimal config + default profile YAML used by every test that exercises
    // the load_config_and_profile path. The active profile must materialize as
    // a profile file under `profiles/` for resolve_profile to succeed.
    const CONFIG_YAML: &str = "apiVersion: cfgd.io/v1alpha1\n\
                               kind: Config\n\
                               metadata:\n  name: t\n\
                               spec:\n  profile: default\n";

    const PROFILE_YAML: &str = "apiVersion: cfgd.io/v1alpha1\n\
                                kind: Profile\n\
                                metadata:\n  name: default\n\
                                spec: {}\n";

    /// Profile that references `test-mod`; used by tests that exercise the
    /// per-module rendering and structured output paths.
    const PROFILE_WITH_MODULE_YAML: &str = "apiVersion: cfgd.io/v1alpha1\n\
                                            kind: Profile\n\
                                            metadata:\n  name: default\n\
                                            spec:\n  modules:\n    - test-mod\n";

    const MODULE_YAML: &str = "apiVersion: cfgd.io/v1alpha1\n\
                               kind: Module\n\
                               metadata:\n  name: test-mod\n\
                               spec:\n  packages:\n    - name: ripgrep\n";

    fn test_cli_for(config_path: std::path::PathBuf, state_dir: &std::path::Path) -> Cli {
        Cli {
            config: config_path,
            profile: None,
            verbose: 0,
            quiet: true,
            no_color: true,
            output: OutputFormatArg(OutputFormat::Table),
            jsonpath: None,
            state_dir: Some(state_dir.to_path_buf()),
            command: None,
        }
    }

    /// Isolated config-dir + state-dir pair with a minimal valid `cfgd.yaml`
    /// and matching `profiles/default.yaml`.
    fn setup_env() -> (tempfile::TempDir, tempfile::TempDir, std::path::PathBuf) {
        let config_dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let config_path = config_dir.path().join("cfgd.yaml");
        std::fs::write(&config_path, CONFIG_YAML).unwrap();
        let profiles_dir = config_dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(profiles_dir.join("default.yaml"), PROFILE_YAML).unwrap();
        std::fs::create_dir_all(config_dir.path().join("modules")).unwrap();
        (config_dir, state_dir, config_path)
    }

    /// Same as `setup_env` but the default profile references `test-mod` and
    /// the corresponding `modules/test-mod/module.yaml` is materialized.
    fn setup_env_with_module() -> (tempfile::TempDir, tempfile::TempDir, std::path::PathBuf) {
        let config_dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let config_path = config_dir.path().join("cfgd.yaml");
        std::fs::write(&config_path, CONFIG_YAML).unwrap();
        let profiles_dir = config_dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(profiles_dir.join("default.yaml"), PROFILE_WITH_MODULE_YAML).unwrap();
        let mod_dir = config_dir.path().join("modules").join("test-mod");
        std::fs::create_dir_all(&mod_dir).unwrap();
        std::fs::write(mod_dir.join("module.yaml"), MODULE_YAML).unwrap();
        (config_dir, state_dir, config_path)
    }

    // --- cmd_status (aggregate) -------------------------------------------

    #[test]
    fn cmd_status_missing_config_returns_err() {
        let state_dir = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let cli = test_cli_for(dir.path().join("nope.yaml"), state_dir.path());
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);

        let err = cmd_status(&cli, &printer, None, false).unwrap_err();
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("not found") || msg.contains("nope.yaml"),
            "expected config-not-found error, got: {err}"
        );
    }

    #[test]
    fn cmd_status_empty_state_renders_no_applies_and_no_drift() {
        let (_cfg_dir, state_dir, config_path) = setup_env();
        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, buf) = Printer::for_test();

        cmd_status(&cli, &printer, None, false).unwrap();

        let output = buf.lock().unwrap();
        assert!(
            output.contains("=== Status ==="),
            "should render Status header, got: {output}"
        );
        assert!(
            output.contains("No applies recorded yet"),
            "empty applies table should print info line, got: {output}"
        );
        assert!(
            output.contains("No drift detected"),
            "empty drift should print success line, got: {output}"
        );
    }

    #[test]
    fn cmd_status_with_apply_record_prints_last_apply_block() {
        let (_cfg_dir, state_dir, config_path) = setup_env();
        let store = open_state_store(Some(state_dir.path())).unwrap();
        store
            .record_apply(
                "default",
                "deadbeef",
                ApplyStatus::Success,
                Some("test apply summary"),
            )
            .unwrap();

        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, buf) = Printer::for_test();

        cmd_status(&cli, &printer, None, false).unwrap();

        let output = buf.lock().unwrap();
        assert!(
            output.contains("Last Apply"),
            "should render Last Apply subheader, got: {output}"
        );
        assert!(
            output.contains("Profile: default"),
            "should print profile key/value, got: {output}"
        );
        assert!(
            output.contains("Status: success"),
            "should print success status, got: {output}"
        );
        assert!(
            output.contains("test apply summary"),
            "should include summary text, got: {output}"
        );
    }

    #[test]
    fn cmd_status_drift_present_renders_warning_line() {
        let (_cfg_dir, state_dir, config_path) = setup_env();
        let store = open_state_store(Some(state_dir.path())).unwrap();
        store
            .record_drift(
                "file",
                "/etc/hosts",
                Some("desired-hash"),
                Some("actual-hash"),
                "local",
            )
            .unwrap();

        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, buf) = Printer::for_test();

        cmd_status(&cli, &printer, None, false).unwrap();

        let output = buf.lock().unwrap();
        assert!(
            !output.contains("No drift detected"),
            "drift recorded — should NOT print all-clear line, got: {output}"
        );
        assert!(
            output.contains("file") && output.contains("/etc/hosts"),
            "drift event should appear in output, got: {output}"
        );
        assert!(
            output.contains("desired-hash") && output.contains("actual-hash"),
            "drift line should include want/have values, got: {output}"
        );
    }

    #[test]
    fn cmd_status_drift_non_local_source_includes_source_tag() {
        let (_cfg_dir, state_dir, config_path) = setup_env();
        let store = open_state_store(Some(state_dir.path())).unwrap();
        store
            .record_drift(
                "package",
                "ripgrep",
                Some("1.0"),
                Some("0.9"),
                "team-config",
            )
            .unwrap();

        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, buf) = Printer::for_test();

        cmd_status(&cli, &printer, None, false).unwrap();

        let output = buf.lock().unwrap();
        // The format string adds " [<source>]" only when source != "local".
        assert!(
            output.contains("[team-config]"),
            "non-local drift should include bracketed source, got: {output}"
        );
    }

    #[test]
    fn cmd_status_managed_resources_renders_table() {
        let (_cfg_dir, state_dir, config_path) = setup_env();
        let store = open_state_store(Some(state_dir.path())).unwrap();
        store
            .upsert_managed_resource("file", "/etc/managed.conf", "local", Some("hashval"), None)
            .unwrap();

        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, buf) = Printer::for_test();

        cmd_status(&cli, &printer, None, false).unwrap();

        let output = buf.lock().unwrap();
        assert!(
            output.contains("Managed Resources"),
            "should print Managed Resources subheader, got: {output}"
        );
        assert!(
            output.contains("/etc/managed.conf"),
            "managed resource row should be present, got: {output}"
        );
    }

    #[test]
    fn cmd_status_exit_code_false_with_drift_returns_ok() {
        // Guard: when --exit-code is not set, drift presence must NOT trigger
        // process::exit. This is the only safe half of the exit-code semantics
        // we can test in-process; the `true` branch would terminate the runner.
        let (_cfg_dir, state_dir, config_path) = setup_env();
        let store = open_state_store(Some(state_dir.path())).unwrap();
        store
            .record_drift("file", "/etc/x", Some("a"), Some("b"), "local")
            .unwrap();

        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, _buf) = Printer::for_test();

        let res = cmd_status(&cli, &printer, None, false);
        assert!(res.is_ok(), "exit_code=false must return Ok, got: {res:?}");
    }

    #[test]
    fn cmd_status_exit_code_true_no_drift_returns_ok() {
        // Complement to the test above: with `exit_code=true` but no drift
        // recorded, the function must not call `process::exit` and must return
        // Ok. This exercises the (exit_code && has_drift) short-circuit gate.
        let (_cfg_dir, state_dir, config_path) = setup_env();
        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, _buf) = Printer::for_test();

        let res = cmd_status(&cli, &printer, None, true);
        assert!(
            res.is_ok(),
            "exit_code=true with no drift must return Ok, got: {res:?}"
        );
    }

    #[test]
    fn cmd_status_json_output_emits_expected_shape() {
        let (_cfg_dir, state_dir, config_path) = setup_env();
        let store = open_state_store(Some(state_dir.path())).unwrap();
        store
            .record_apply("default", "abc123", ApplyStatus::Success, Some("ok"))
            .unwrap();
        store
            .record_drift("file", "/etc/foo", Some("want"), Some("have"), "local")
            .unwrap();

        let mut cli = test_cli_for(config_path, state_dir.path());
        cli.output = OutputFormatArg(OutputFormat::Json);
        let (printer, buf) = Printer::for_test_with_format(OutputFormat::Json);

        cmd_status(&cli, &printer, None, false).unwrap();

        // load_config_and_profile captures Config:/Profile: key-value lines
        // into the same buffer before the JSON is emitted; slice from the
        // first '{' so serde_json sees a clean object.
        let captured = buf.lock().unwrap().clone();
        let json_start = captured
            .find('{')
            .unwrap_or_else(|| panic!("no JSON object in output: {captured}"));
        let parsed: serde_json::Value = serde_json::from_str(captured[json_start..].trim())
            .unwrap_or_else(|e| panic!("invalid JSON: {e}, got: {captured}"));
        assert!(
            parsed["lastApply"].is_object(),
            "lastApply should be an object, got: {parsed}"
        );
        assert_eq!(parsed["lastApply"]["profile"], "default");
        let drift = parsed["drift"].as_array().expect("drift array");
        assert_eq!(drift.len(), 1, "expected 1 drift entry, got: {parsed}");
        assert_eq!(drift[0]["resourceType"], "file");
        assert_eq!(drift[0]["resourceId"], "/etc/foo");
        // Empty arrays should still be present (not omitted).
        assert!(parsed["sources"].is_array());
        assert!(parsed["pendingDecisions"].is_array());
        assert!(parsed["modules"].is_array());
        assert!(parsed["managedResources"].is_array());
    }

    #[test]
    fn cmd_status_module_filter_routes_to_per_module_path() {
        // When `module_filter` is Some, cmd_status delegates to
        // cmd_status_module — the aggregate "Status" header must NOT appear.
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let (_cfg_dir, state_dir, config_path) = setup_env_with_module();

        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, buf) = Printer::for_test();

        cmd_status(&cli, &printer, Some("test-mod"), false).unwrap();

        let output = buf.lock().unwrap();
        // Per-module header is "Status: <name>" — must be present.
        assert!(
            output.contains("Status: test-mod"),
            "should route to per-module header, got: {output}"
        );
        // Aggregate-only sections (no apply record was made → 'No applies'
        // would have appeared in the main path) must NOT appear.
        assert!(
            !output.contains("No applies recorded yet"),
            "should not fall through to aggregate path, got: {output}"
        );
    }

    // --- cmd_status_module ------------------------------------------------

    #[test]
    fn cmd_status_module_unknown_module_table_prints_not_found() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let (_cfg_dir, state_dir, config_path) = setup_env();

        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, buf) = Printer::for_test();

        cmd_status_module(&cli, &printer, "ghost").unwrap();

        let output = buf.lock().unwrap();
        assert!(
            output.contains("Status: ghost"),
            "should print module header, got: {output}"
        );
        assert!(
            output.contains("not found"),
            "unknown module should print not-found info, got: {output}"
        );
    }

    #[test]
    fn cmd_status_module_unknown_module_json_emits_not_found_shape() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let (_cfg_dir, state_dir, config_path) = setup_env();

        let mut cli = test_cli_for(config_path, state_dir.path());
        cli.output = OutputFormatArg(OutputFormat::Json);
        let (printer, buf) = Printer::for_test_with_format(OutputFormat::Json);

        cmd_status_module(&cli, &printer, "ghost").unwrap();

        let captured = buf.lock().unwrap().clone();
        let parsed: serde_json::Value = serde_json::from_str(captured.trim())
            .unwrap_or_else(|e| panic!("invalid JSON: {e}, got: {captured}"));
        assert_eq!(parsed["name"], "ghost");
        assert_eq!(parsed["status"], "not found");
        assert_eq!(parsed["packages"], 0);
        assert_eq!(parsed["files"], 0);
        assert!(parsed["lastApplied"].is_null());
    }

    #[test]
    fn cmd_status_module_known_module_with_state_renders_details() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let (_cfg_dir, state_dir, config_path) = setup_env_with_module();

        // Pre-populate module state.
        let store = open_state_store(Some(state_dir.path())).unwrap();
        store
            .upsert_module_state(
                "test-mod",
                None,
                "pkg-hash-xyz",
                "files-hash-abc",
                None,
                "installed",
            )
            .unwrap();

        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, buf) = Printer::for_test();

        cmd_status_module(&cli, &printer, "test-mod").unwrap();

        let output = buf.lock().unwrap();
        assert!(
            output.contains("Status: test-mod"),
            "should print module header, got: {output}"
        );
        assert!(
            output.contains("Packages: 1"),
            "module declares 1 package, got: {output}"
        );
        assert!(
            output.contains("Status: installed"),
            "should print state-store status, got: {output}"
        );
        assert!(
            output.contains("pkg-hash-xyz"),
            "should print packages hash, got: {output}"
        );
        assert!(
            output.contains("files-hash-abc"),
            "should print files hash, got: {output}"
        );
    }

    #[test]
    fn cmd_status_module_without_state_record_prints_not_applied() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let (_cfg_dir, state_dir, config_path) = setup_env_with_module();

        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, buf) = Printer::for_test();

        cmd_status_module(&cli, &printer, "test-mod").unwrap();

        let output = buf.lock().unwrap();
        assert!(
            output.contains("Status: not applied"),
            "no state-store record should produce 'not applied', got: {output}"
        );
    }

    #[test]
    fn cmd_status_module_renders_deployed_files_section() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let (_cfg_dir, state_dir, config_path) = setup_env_with_module();

        // Materialize an existing deployed file so the path-exists branch runs
        // (and a separate missing-file path so the error-line branch runs).
        let real_file = tmp_home.path().join("real.conf");
        std::fs::write(&real_file, b"x").unwrap();

        let store = open_state_store(Some(state_dir.path())).unwrap();
        let apply_id = store
            .record_apply("default", "h", ApplyStatus::Success, None)
            .unwrap();
        store
            .upsert_module_file(
                "test-mod",
                real_file.to_str().unwrap(),
                "hash-exists",
                "copy",
                apply_id,
            )
            .unwrap();
        store
            .upsert_module_file(
                "test-mod",
                "/nonexistent/missing.conf",
                "hash-missing",
                "copy",
                apply_id,
            )
            .unwrap();

        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, buf) = Printer::for_test();

        cmd_status_module(&cli, &printer, "test-mod").unwrap();

        let output = buf.lock().unwrap();
        assert!(
            output.contains("Deployed Files"),
            "deployed files section should be present, got: {output}"
        );
        assert!(
            output.contains(real_file.to_str().unwrap()),
            "existing file should appear as success, got: {output}"
        );
        assert!(
            output.contains("/nonexistent/missing.conf") && output.contains("(missing)"),
            "missing file should be flagged, got: {output}"
        );
    }

    #[test]
    fn cmd_status_module_known_module_json_shape() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let (_cfg_dir, state_dir, config_path) = setup_env_with_module();

        let store = open_state_store(Some(state_dir.path())).unwrap();
        store
            .upsert_module_state("test-mod", None, "pkgh", "fileh", None, "installed")
            .unwrap();

        let mut cli = test_cli_for(config_path, state_dir.path());
        cli.output = OutputFormatArg(OutputFormat::Json);
        let (printer, buf) = Printer::for_test_with_format(OutputFormat::Json);

        cmd_status_module(&cli, &printer, "test-mod").unwrap();

        let captured = buf.lock().unwrap().clone();
        let parsed: serde_json::Value = serde_json::from_str(captured.trim())
            .unwrap_or_else(|e| panic!("invalid JSON: {e}, got: {captured}"));
        assert_eq!(parsed["name"], "test-mod");
        assert_eq!(parsed["packages"], 1);
        assert_eq!(parsed["files"], 0);
        assert_eq!(parsed["status"], "installed");
        assert!(
            parsed["lastApplied"].is_string(),
            "lastApplied should be the installed_at timestamp, got: {parsed}"
        );
        assert!(parsed["depends"].is_array());
    }
}
