use super::*;
use cfgd_core::PathDisplayExt;
use cfgd_core::output::{Doc, Printer, Role, renderer::Table};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusOutput {
    pub last_apply: Option<cfgd_core::state::ApplyRecord>,
    pub drift: Vec<cfgd_core::state::DriftEvent>,
    pub sources: Vec<cfgd_core::state::ConfigSourceRecord>,
    pub pending_decisions: Vec<cfgd_core::state::PendingDecision>,
    pub modules: Vec<ModuleStatusEntry>,
    pub managed_resources: Vec<cfgd_core::state::ManagedResource>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleStatusEntry {
    pub name: String,
    pub packages: usize,
    pub files: usize,
    pub status: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleStatus {
    pub name: String,
    pub packages: usize,
    pub files: usize,
    pub depends: Vec<String>,
    pub status: String,
    pub last_applied: Option<String>,
}

fn apply_status_str(s: &cfgd_core::state::ApplyStatus) -> &'static str {
    match s {
        cfgd_core::state::ApplyStatus::Success => "success",
        cfgd_core::state::ApplyStatus::Partial => "partial",
        cfgd_core::state::ApplyStatus::Failed => "failed",
        cfgd_core::state::ApplyStatus::InProgress => "in_progress",
        cfgd_core::state::ApplyStatus::Aborted => "aborted",
    }
}

/// Build the fleet-wide `cfgd status` Doc. Caller supplies the precomputed
/// payload and the configured `SourceSpec` list so the renderer can show
/// "not yet fetched" rows for sources without state records.
pub fn build_fleet_status_doc(
    output: &StatusOutput,
    configured_sources: &[String],
    config_path: &Path,
    profile_name: &str,
) -> Doc {
    let mut doc = Doc::new()
        .heading("Status")
        .kv("Config", config_path.display_posix())
        .kv("Profile", profile_name);

    match &output.last_apply {
        Some(last) => {
            doc = doc.section("Last Apply", |s| {
                let mut s = s
                    .kv("Time", &last.timestamp)
                    .kv("Profile", &last.profile)
                    .kv("Result", apply_status_str(&last.status));
                if let Some(summary) = &last.summary {
                    s = s.kv("Summary", summary);
                }
                s
            });
        }
        None => {
            doc = doc.status(Role::Info, "No applies recorded yet");
        }
    }

    doc = if output.drift.is_empty() {
        doc.section("Drift", |s| s.status(Role::Ok, "No drift detected"))
    } else {
        doc.section("Drift", |s| {
            output.drift.iter().fold(s, |s, event| {
                let subject = format!(
                    "{} {} — want: {}, have: {}",
                    event.resource_type,
                    event.resource_id,
                    event.expected.as_deref().unwrap_or("?"),
                    event.actual.as_deref().unwrap_or("?"),
                );
                if event.source != "local" {
                    // Source attribution renders in `secondary` (pink/magenta)
                    // at end-of-subject; the StatusBuilder API guarantees the
                    // label lands last so the inner SGR reset is never
                    // followed by outer-role-styled text.
                    let label_text = format!("[{}]", event.source);
                    s.status_with(Role::Warn, subject, |f| {
                        f.label(Role::Secondary, label_text)
                    })
                } else {
                    s.status(Role::Warn, subject)
                }
            })
        })
    };

    if !configured_sources.is_empty() {
        doc = doc.section("Config Sources", |s| {
            if output.sources.is_empty() {
                configured_sources
                    .iter()
                    .fold(s, |s, name| s.kv(name, "not yet fetched"))
            } else {
                let mut t = Table::new(["Source", "Status", "Version", "Last Fetched"]);
                for rec in &output.sources {
                    t = t.row([
                        rec.name.clone(),
                        rec.status.clone(),
                        rec.source_version.clone().unwrap_or_else(|| "-".into()),
                        rec.last_fetched.clone().unwrap_or_else(|| "never".into()),
                    ]);
                }
                s.table(t)
            }
        });
    }

    doc = doc.section_if_nonempty(
        "Pending Decisions",
        &output.pending_decisions,
        |s, decisions| {
            let mut by_source: std::collections::BTreeMap<
                &str,
                Vec<&cfgd_core::state::PendingDecision>,
            > = std::collections::BTreeMap::new();
            for d in decisions {
                by_source.entry(&d.source).or_default().push(d);
            }
            by_source.into_iter().fold(s, |s, (source_name, items)| {
                let count = items.len();
                let plural = if count == 1 { "" } else { "s" };
                s.subsection(source_name.to_string(), |sub| {
                    let sub = sub.status(Role::Info, format!("{count} pending item{plural}"));
                    items.iter().fold(sub, |sub, item| {
                        sub.status(
                            Role::Info,
                            format!(
                                "{} {} — {} ({})",
                                item.tier, item.resource, item.summary, item.action
                            ),
                        )
                    })
                })
            })
        },
    );

    doc = doc.section_if_nonempty("Modules", &output.modules, |s, mods| {
        mods.iter().fold(s, |s, m| {
            let summary = format!("{} pkgs, {} files", m.packages, m.files);
            let role = match m.status.as_str() {
                "installed" => Role::Ok,
                "not applied" | "not yet applied" => Role::Info,
                _ => Role::Warn,
            };
            let suffix = if m.status == "not applied" {
                "not yet applied".to_string()
            } else {
                m.status.clone()
            };
            s.status(role, format!("{}: {}, {}", m.name, summary, suffix))
        })
    });

    doc = doc.section_if_nonempty(
        "Managed Resources",
        &output.managed_resources,
        |s, items| {
            let mut t = Table::new(["Type", "Resource", "Source"]);
            for r in items {
                t = t.row([
                    r.resource_type.clone(),
                    r.resource_id.clone(),
                    r.source.clone(),
                ]);
            }
            s.table(t)
        },
    );

    doc.with_data(output)
}

/// Build the per-module `cfgd status <module>` Doc.
/// `deployed_files` is a list of (path, exists) pairs.
pub fn build_module_status_doc(output: &ModuleStatus, deployed_files: &[(String, bool)]) -> Doc {
    let mut doc = Doc::new()
        .heading(format!("Status: {}", output.name))
        .kv("Packages", output.packages.to_string())
        .kv("Files", output.files.to_string());

    if !output.depends.is_empty() {
        doc = doc.kv("Dependencies", output.depends.join(", "));
    }

    doc = doc.kv("Status", &output.status);
    if let Some(last) = &output.last_applied {
        doc = doc.kv("Last applied", last);
    }

    doc = doc.section_if_nonempty("Deployed Files", deployed_files, |s, files| {
        files.iter().fold(s, |s, (path, exists)| {
            if *exists {
                s.status(Role::Ok, path)
            } else {
                s.status(Role::Fail, format!("{} (missing)", path))
            }
        })
    });

    doc.with_data(output)
}

/// Doc for the `cfgd status <module>` not-found path. Renders the module
/// header and an info note; structured consumers get a payload with packages=0
/// and `status: "not found"`. Returns Ok(()) — no main-side error rendering.
pub fn build_module_status_not_found_doc(name: &str) -> Doc {
    let payload = ModuleStatus {
        name: name.to_string(),
        packages: 0,
        files: 0,
        depends: Vec::new(),
        status: "not found".into(),
        last_applied: None,
    };
    Doc::new()
        .heading(format!("Status: {}", name))
        .status(Role::Info, format!("Module '{}' not found", name))
        .with_data(&payload)
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

    let (cfg, profile_name, local_resolved) = load_config_and_profile(cli)?;
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

    let config_dir = config_dir(cli);

    // Compose with sources (cache-only — read paths stay offline) and resolve the
    // effective module set once, so the module dashboard and the `-e` live scan
    // both reflect the same source-composed desired state that `apply` writes.
    let desired = resolve_desired_state(cli, &cfg, &local_resolved, None, printer, false)?;
    let mut resolved = desired.resolved;
    let resolved_modules = desired.modules;

    let state_map = module_state_map(&state);
    let module_entries: Vec<ModuleStatusEntry> = resolved_modules
        .iter()
        .map(|module| {
            let status = state_map
                .get(&module.name)
                .map(|s| s.status.clone())
                .unwrap_or_else(|| "not applied".into());
            ModuleStatusEntry {
                name: module.name.clone(),
                packages: module.packages.len(),
                files: module.files.len(),
                status,
            }
        })
        .collect();

    let configured_source_names: Vec<String> =
        cfg.spec.sources.iter().map(|s| s.name.clone()).collect();

    let mut output = StatusOutput {
        last_apply,
        drift: drift_events,
        sources: source_records,
        pending_decisions: pending,
        modules: module_entries,
        managed_resources: resources,
    };

    // Plain `status` (no --exit-code) keeps the fast RECORDED-drift dashboard by
    // deliberate design. The --exit-code gate, however, must reflect REALITY: a
    // host with no daemon and no prior scan has zero recorded events even when a
    // managed file was just edited out-of-band. So in `-e` mode run the LIVE,
    // read-only scan (never recording — the same checks `diff`/`verify` run)
    // BEFORE emitting, fold its findings into the displayed Drift section, then
    // exit 5 if any drift. This keeps the human verdict and the exit code in
    // agreement instead of printing "No drift detected" alongside exit 5.
    let live_drift = if exit_code {
        packages::resolve_manifest_packages(&mut resolved.merged.packages, &config_dir)?;
        let mut registry = build_registry_with_profile(&resolved.merged.packages);
        registry.set_system_config_dir(&config_dir);
        let cfgd_installed = cfgd_installed_packages(&state)?;
        let drift = super::live_drift::live_drift_results(
            &config_dir,
            &resolved,
            &registry,
            &resolved_modules,
            &cfgd_installed,
        )?;
        for r in &drift {
            output.drift.push(cfgd_core::state::DriftEvent {
                id: 0,
                timestamp: cfgd_core::utc_now_iso8601(),
                resource_type: r.resource_type.clone(),
                resource_id: r.resource_id.clone(),
                expected: Some(r.expected.clone()),
                actual: Some(r.actual.clone()),
                resolved_by: None,
                source: "local".to_string(),
            });
        }
        drift
    } else {
        Vec::new()
    };

    printer.emit(build_fleet_status_doc(
        &output,
        &configured_source_names,
        &cli.config,
        &profile_name,
    ));

    if exit_code && !live_drift.is_empty() {
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
    // queries a single named module, so a missing cache dir means the query
    // cannot be answered, and it must error rather than silently claim the
    // module was not found.
    let cache_base = modules::default_module_cache_dir()?;
    let all_modules = modules::load_all_modules(&config_dir, &cache_base, &[], printer)?;

    let module = match all_modules.get(mod_name) {
        Some(m) => m,
        None => {
            printer.emit(build_module_status_not_found_doc(mod_name));
            return Ok(());
        }
    };

    let state = open_state_store(cli.state_dir.as_deref())?;
    let state_rec = state.module_state_by_name(mod_name)?;

    let status = state_rec
        .as_ref()
        .map(|s| s.status.clone())
        .unwrap_or_else(|| "not applied".into());
    let last_applied = state_rec.as_ref().map(|s| s.installed_at.clone());

    let output = ModuleStatus {
        name: mod_name.to_string(),
        packages: module.spec.packages.len(),
        files: module.spec.files.len(),
        depends: module.spec.depends.clone(),
        status,
        last_applied,
    };

    let deployed_files: Vec<(String, bool)> = state
        .module_deployed_files(mod_name)?
        .into_iter()
        .map(|f| {
            let exists = std::path::Path::new(&f.file_path).exists();
            (f.file_path, exists)
        })
        .collect();

    printer.emit(build_module_status_doc(&output, &deployed_files));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cfgd_core::output::Printer;
    use cfgd_core::output::Verbosity;
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
            output: OutputFormatArg(cfgd_core::output::OutputFormat::Table),
            list_envelope: false,
            jsonpath: None,
            state_dir: Some(state_dir.to_path_buf()),
            command: None,
        }
    }

    fn test_printers() -> (Printer, std::sync::Arc<std::sync::Mutex<String>>) {
        Printer::for_test_at(Verbosity::Normal)
    }

    fn test_printers_json() -> (Printer, std::sync::Arc<std::sync::Mutex<String>>) {
        Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json)
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
        let (printer, _) = test_printers();

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
        let (printer, buf) = test_printers();

        cmd_status(&cli, &printer, None, false).unwrap();
        drop(printer);

        let output = buf.lock().unwrap();
        assert!(
            output.contains("Status"),
            "should render Status heading, got: {output}"
        );
        assert!(
            output.contains("No applies recorded yet"),
            "empty applies state should render info line, got: {output}"
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
        let (printer, buf) = test_printers();

        cmd_status(&cli, &printer, None, false).unwrap();
        drop(printer);

        let output = buf.lock().unwrap();
        assert!(
            output.contains("Last Apply"),
            "should render Last Apply section, got: {output}"
        );
        assert!(
            output.contains("default"),
            "should print profile, got: {output}"
        );
        assert!(
            output.contains("success"),
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
        let (printer, buf) = test_printers();

        cmd_status(&cli, &printer, None, false).unwrap();
        drop(printer);

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
        let (printer, buf) = test_printers();

        cmd_status(&cli, &printer, None, false).unwrap();
        drop(printer);

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
        let (printer, buf) = test_printers();

        cmd_status(&cli, &printer, None, false).unwrap();
        drop(printer);

        let output = buf.lock().unwrap();
        assert!(
            output.contains("Managed Resources"),
            "should print Managed Resources section, got: {output}"
        );
        assert!(
            output.contains("/etc/managed.conf"),
            "managed resource row should be present, got: {output}"
        );
    }

    #[test]
    fn cmd_status_exit_code_false_with_drift_returns_ok() {
        // Guard: when --exit-code is not set, drift presence must NOT trigger
        // process::exit. Only the non-exiting half is testable in-process; the
        // drift-present branch would terminate the test runner via process::exit.
        let (_cfg_dir, state_dir, config_path) = setup_env();
        let store = open_state_store(Some(state_dir.path())).unwrap();
        store
            .record_drift("file", "/etc/x", Some("a"), Some("b"), "local")
            .unwrap();

        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, _) = test_printers();

        let res = cmd_status(&cli, &printer, None, false);
        assert!(res.is_ok(), "exit_code=false must return Ok, got: {res:?}");
    }

    #[test]
    fn cmd_status_exit_code_true_no_drift_returns_ok() {
        // Complement to the test above: with `exit_code=true` but a clean host,
        // the live-scan gate finds no drift, so the function must not call
        // `process::exit` and must return Ok.
        let (_cfg_dir, state_dir, config_path) = setup_env();
        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, _) = test_printers();

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
        cli.output = OutputFormatArg(cfgd_core::output::OutputFormat::Json);
        let (printer, buf) = test_printers_json();

        cmd_status(&cli, &printer, None, false).unwrap();
        drop(printer);

        let captured = buf.lock().unwrap().clone();
        let parsed: serde_json::Value = serde_json::from_str(captured.trim())
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
        // cmd_status_module — the aggregate "Status" heading must NOT appear.
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let (_cfg_dir, state_dir, config_path) = setup_env_with_module();

        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, buf) = test_printers();

        cmd_status(&cli, &printer, Some("test-mod"), false).unwrap();
        drop(printer);

        let output = buf.lock().unwrap();
        // Per-module heading is "Status: <name>" — must be present.
        assert!(
            output.contains("Status: test-mod"),
            "should route to per-module heading, got: {output}"
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
        let (printer, buf) = test_printers();

        cmd_status_module(&cli, &printer, "ghost").unwrap();
        drop(printer);

        let output = buf.lock().unwrap();
        assert!(
            output.contains("Status: ghost"),
            "should print module heading, got: {output}"
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
        cli.output = OutputFormatArg(cfgd_core::output::OutputFormat::Json);
        let (printer, buf) = test_printers_json();

        cmd_status_module(&cli, &printer, "ghost").unwrap();
        drop(printer);

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
        let (printer, buf) = test_printers();

        cmd_status_module(&cli, &printer, "test-mod").unwrap();
        drop(printer);

        let output = buf.lock().unwrap();
        assert!(
            output.contains("Status: test-mod"),
            "should print module heading, got: {output}"
        );
        assert!(
            output.contains("Packages") && output.contains('1'),
            "module declares 1 package, got: {output}"
        );
        assert!(
            output.contains("installed"),
            "should print state-store status, got: {output}"
        );
    }

    #[test]
    fn cmd_status_module_without_state_record_prints_not_applied() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let (_cfg_dir, state_dir, config_path) = setup_env_with_module();

        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, buf) = test_printers();

        cmd_status_module(&cli, &printer, "test-mod").unwrap();
        drop(printer);

        let output = buf.lock().unwrap();
        assert!(
            output.contains("not applied"),
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
        let (printer, buf) = test_printers();

        cmd_status_module(&cli, &printer, "test-mod").unwrap();
        drop(printer);

        let output = buf.lock().unwrap();
        assert!(
            output.contains("Deployed Files"),
            "deployed files section should be present, got: {output}"
        );
        assert!(
            output.contains(real_file.to_str().unwrap()),
            "existing file should appear, got: {output}"
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
        cli.output = OutputFormatArg(cfgd_core::output::OutputFormat::Json);
        let (printer, buf) = test_printers_json();

        cmd_status_module(&cli, &printer, "test-mod").unwrap();
        drop(printer);

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
