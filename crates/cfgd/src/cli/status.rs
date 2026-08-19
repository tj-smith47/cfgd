use super::*;
use cfgd_core::PathDisplayExt;
use cfgd_core::config::LOCAL_LAYER;
use cfgd_core::output::{Doc, Printer, Role, condense_script_label, renderer::Table};
use cfgd_core::reconciler::Owner;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusOutput {
    pub last_apply: Option<cfgd_core::state::ApplyRecord>,
    pub drift: Vec<cfgd_core::state::DriftEvent>,
    pub sources: Vec<cfgd_core::state::ConfigSourceRecord>,
    pub pending_decisions: Vec<cfgd_core::state::PendingDecision>,
    pub modules: Vec<ModuleStatusEntry>,
    pub managed_resources: Vec<cfgd_core::state::ManagedResource>,
    /// Source batches no decision row can name (a dotted custom manager) —
    /// withheld from every plan fail-closed, so the dashboard names them here
    /// instead of showing clean-empty. Same lines the `plan` payload's
    /// `warnings` carries; absent when there are none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// True when the source-decision classification failed and
    /// `pendingDecisions` is missing the classified-but-unrecorded items — a
    /// degraded listing is otherwise indistinguishable from a clean empty one
    /// to a `-o json` consumer.
    pub classification_degraded: bool,
    /// The machine-stable cause class, present only when degraded — the
    /// reason string beside it is the human detail and carries no stability
    /// promise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classification_degraded_code: Option<super::output_types::ClassificationDegradedCode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classification_degraded_reason: Option<String>,
    /// Whether `drift` is the verdict of a LIVE scan of this machine or the
    /// events something previously recorded. Plain `status` is the fast
    /// recorded-drift dashboard, so on a host with no daemon its `drift` is
    /// empty however far the machine has drifted; only `--exit-code` scans.
    /// A consumer differencing an empty list needs to know which of those two
    /// it is holding, and the human line says the same thing in words.
    pub drift_checked_live: bool,
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
    /// Live drift found for this module's files and packages. Always empty
    /// unless `--exit-code` requested the live scan — see `drift_checked_live`.
    pub drift: Vec<cfgd_core::state::DriftEvent>,
    /// Whether `drift` is the verdict of a live scan of this module or just
    /// an unchecked empty default. Mirrors `StatusOutput::drift_checked_live`
    /// so the two `-o json` shapes read the same way.
    pub drift_checked_live: bool,
}

/// Render the "Drift" section shared by the fleet-wide and per-module status
/// docs. Both feed it the same `DriftEvent` shape, so a resource-id
/// condensing rule or an attribution label added here reaches both surfaces
/// without a second copy drifting out of sync.
fn render_drift_section(
    doc: Doc,
    drift: &[cfgd_core::state::DriftEvent],
    checked_live: bool,
) -> Doc {
    if drift.is_empty() {
        // Only the live scan may claim a detection. The recorded dashboard has
        // asked nothing of the machine, and "No drift detected" over a host
        // whose last apply left a declared package uninstalled is an assurance
        // no query backs.
        let subject = if checked_live {
            "No drift detected"
        } else {
            "No drift recorded — `cfgd diff` checks the live machine"
        };
        doc.section("Drift", |s| s.status(Role::Ok, subject))
    } else {
        doc.section("Drift", |s| {
            drift.iter().fold(s, |s, event| {
                // A "script" / "Running script" resource_id is the raw
                // run_str body (preserved byte-identical for UPSERT matching
                // against prior drift rows) — condense only here, at the
                // point it enters a status subject, so a multi-line inline
                // script never lands raw. Two type strings exist because two
                // producers persist script actions: `apply_script_action`
                // (main pre/post-apply phase scripts, format.rs's
                // `format_action_description`) stamps "script"; `execute_script`
                // (onChange / module-onChange scripts, reconciler/scripts.rs)
                // stamps "Running script: {body}" — both must condense here.
                let display_id =
                    if event.resource_type == "script" || event.resource_type == "Running script" {
                        condense_script_label(&event.resource_id)
                    } else {
                        event.resource_id.clone()
                    };
                let subject = format!("{} {}", event.resource_type, display_id);
                let expected = event.expected.as_deref().unwrap_or("?");
                let actual = event.actual.as_deref().unwrap_or("?");
                if event.source != LOCAL_LAYER {
                    // Source attribution renders in `secondary` (pink/magenta)
                    // at end-of-subject; the StatusBuilder API guarantees the
                    // label lands last so the inner SGR reset is never
                    // followed by outer-role-styled text. The token is the
                    // vocabulary `cfgd sync` and `cfgd source *` head their
                    // groups with, so a reader carries one spelling across the
                    // three surfaces that name a source.
                    let label_text = Owner::source(&event.source).token();
                    s.status_with(Role::Warn, subject, |f| {
                        f.drift(expected, actual).label(Role::Secondary, label_text)
                    })
                } else {
                    s.status_with(Role::Warn, subject, |f| f.drift(expected, actual))
                }
            })
        })
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
                    .kv("Result", last.status.display_str());
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

    doc = render_drift_section(doc, &output.drift, output.drift_checked_live);

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
        super::build_pending_decisions_table_section,
    );

    // Rendered beside the pending rows those batches would otherwise be:
    // "why isn't requests installed?" must be answerable from the dashboard,
    // not only from a plan/apply run header.
    doc = output
        .warnings
        .iter()
        .fold(doc, |d, w| d.status(Role::Warn, w));

    doc = doc.section_if_nonempty("Modules", &output.modules, |s, mods| {
        mods.iter().fold(s, |s, m| {
            // Fixed units, so they agree with their own count: one package is
            // `1 pkg`, not `1 pkgs`.
            let summary = format!(
                "{} pkg{}, {} file{}",
                m.packages,
                if m.packages == 1 { "" } else { "s" },
                m.files,
                if m.files == 1 { "" } else { "s" }
            );
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
            // Subject is the owner token, exactly as the tree that applied the
            // module heads its group; the counts and the state are what the
            // line reports about it.
            s.status_with(role, Owner::module(&m.name).token(), |f| {
                f.detail(format!("{summary}, {suffix}"))
            })
        })
    });

    doc = doc.section_if_nonempty(
        "Managed Resources",
        &output.managed_resources,
        |s, items| {
            let mut t = Table::new(["Type", "Resource", "Source"]);
            for r in items {
                // Same rationale as the Drift section above: condense a
                // "script" / "Running script" resource_id only for this
                // table cell, never the stored id itself.
                let display_id =
                    if r.resource_type == "script" || r.resource_type == "Running script" {
                        condense_script_label(&r.resource_id)
                    } else {
                        r.resource_id.clone()
                    };
                t = t.row([r.resource_type.clone(), display_id, r.source.clone()]);
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
        .heading_title("Status", &output.name)
        .kv("Packages", output.packages.to_string())
        .kv("Files", output.files.to_string());

    if !output.depends.is_empty() {
        doc = doc.kv("Dependencies", output.depends.join(", "));
    }

    doc = doc.kv("Status", &output.status);
    if let Some(last) = &output.last_applied {
        doc = doc.kv("Last applied", last);
    }

    doc = render_drift_section(doc, &output.drift, output.drift_checked_live);

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
        drift: Vec::new(),
        drift_checked_live: false,
    };
    Doc::new()
        .heading_title("Status", name)
        .status(Role::Info, format!("Module '{}' not found", name))
        .with_data(&payload)
}

pub(super) fn cmd_status(
    cli: &Cli,
    printer: &Printer,
    module_filter: Option<&str>,
    exit_code: bool,
) -> anyhow::Result<()> {
    let ctx = RunContext::new(cli, printer);
    if let Some(mod_name) = module_filter {
        return cmd_status_module(&ctx, mod_name, exit_code);
    }

    let (cfg, profile_name, local_resolved) = ctx.config_and_profile()?;
    let state = ctx.state()?;

    let last_apply = state.last_apply()?;
    let drift_events = state.unresolved_drift()?;
    let source_records = if !cfg.spec.sources.is_empty() {
        state.config_sources()?
    } else {
        vec![]
    };
    // Only rows `cfgd decide` can still act on: a decision outliving the source
    // that raised it withholds nothing from a plan, so listing it here would
    // report work awaiting an answer that no answer can release.
    let mut pending = reconciler::Subscriptions::known(cfg.spec.sources.iter().map(|s| &s.name))
        .answerable(state.pending_decisions()?);
    let resources = state.managed_resources()?;

    let config_dir = config_dir(cli);

    // Compose with sources (cache-only — read paths stay offline) and resolve the
    // effective module set once, so the module dashboard and the `-e` live scan
    // both reflect the same source-composed desired state that `apply` writes.
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
    // Taken ONLY for the `-e` live scan below, the one half that reads a
    // registry: a plain `cfgd status` is an offline dashboard, and building a
    // registry it never reads would construct every package manager and
    // configurator the host supports for nothing. Taken here rather than at the
    // scan because the two field moves below are partial moves out of `desired`,
    // which block the `&mut self` the accessor needs — and `Some` exactly when
    // `exit_code`, so the scan below can bind it instead of re-testing the flag.
    let registry = exit_code.then(|| desired.take_registry(cfg));
    let mut resolved = desired.resolved;
    let resolved_modules = desired.modules;

    // The plan withholds items no run has recorded a row for yet; a dashboard
    // that hides them contradicts the plan it summarizes. Same classification
    // source `plan` reads, still read-only — the `id` 0 rows mark items whose
    // row `cfgd decide` (or the next apply/tick) will mint. Unlike the gate in
    // plan/apply, a dashboard DEGRADES rather than dying: a classification
    // failure (a malformed package manifest, say) costs the unrecorded rows
    // and says so, never the whole status surface. And with no sources there
    // is nothing to classify, so none of the classification's work runs.
    let mut classification_degraded: Option<(
        super::output_types::ClassificationDegradedCode,
        String,
    )> = None;
    let mut warnings: Vec<String> = Vec::new();
    if !cfg.spec.sources.is_empty() {
        // The dashboard enumerates no package state (it is offline by design),
        // so the classification sees an empty observation and auto-accepts
        // nothing — installed-but-undecided items keep their pending rows
        // here and are released by the next plan/apply/tick, which does
        // enumerate.
        match plan_ops::withheld_for_run(
            &ctx,
            state,
            cfg,
            &resolved,
            true,
            plan_ops::DecisionWrites::ReadOnly,
            &reconciler::ActualPackages::default(),
        ) {
            Ok((withheld, _review)) => {
                warnings = withheld.undecidable.iter().map(|b| b.warning()).collect();
                pending.extend(withheld.pending.into_iter().filter(|d| d.id == 0));
            }
            Err(e) => {
                let code = super::output_types::ClassificationDegradedCode::from_error(&e);
                let reason = cfgd_core::output::collapse_to_subject_line(format!("{e:#}"));
                printer.status_simple(
                    Role::Warn,
                    format!("Source decisions not classified: {reason}"),
                );
                classification_degraded = Some((code, reason));
            }
        }
    }

    let state_map = module_state_map(state);
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
        warnings,
        classification_degraded: classification_degraded.is_some(),
        classification_degraded_code: classification_degraded.as_ref().map(|(c, _)| *c),
        classification_degraded_reason: classification_degraded.map(|(_, r)| r),
        drift_checked_live: exit_code,
    };

    // Plain `status` (no --exit-code) keeps the fast RECORDED-drift dashboard by
    // deliberate design. The --exit-code gate, however, must reflect REALITY: a
    // host with no daemon and no prior scan has zero recorded events even when a
    // managed file was just edited out-of-band. So in `-e` mode run the LIVE,
    // read-only scan (never recording — the same checks `diff`/`verify` run)
    // BEFORE emitting, fold its findings into the displayed Drift section, then
    // exit 5 if any drift. This keeps the human verdict and the exit code in
    // agreement instead of printing "No drift detected" alongside exit 5.
    let live_drift = if let Some(mut registry) = registry {
        ctx.resolve_manifest_packages(&mut resolved.merged.packages)?;
        registry.set_system_config_dir(&config_dir);
        let cfgd_installed = cfgd_installed_packages(state)?;
        let pkg_cx = cfgd_core::providers::PackageContext::new(printer, state);
        let drift = super::live_drift::live_drift_results(
            &config_dir,
            &resolved,
            &registry,
            &resolved_modules,
            &cfgd_installed,
            &pkg_cx,
        )?;
        for r in &drift {
            output.drift.push(super::live_drift::drift_event_from(r));
        }
        drift
    } else {
        Vec::new()
    };

    printer.emit(build_fleet_status_doc(
        &output,
        &configured_source_names,
        &cli.config,
        profile_name,
    ));

    if exit_code && !live_drift.is_empty() {
        cfgd_core::exit::ExitCode::DriftDetected.exit();
    }

    Ok(())
}

pub(super) fn cmd_status_module(
    ctx: &RunContext<'_>,
    mod_name: &str,
    exit_code: bool,
) -> anyhow::Result<()> {
    let cli = ctx.cli();
    let printer = ctx.printer();
    let config_dir = ctx.config_dir();
    // Propagate (vs. unwrap_or_default in cmd_status): the module-scoped path
    // queries a single named module, so a missing cache dir means the query
    // cannot be answered, and it must error rather than silently claim the
    // module was not found.
    let cache_base = module_cache_dir(cli)?;
    let all_modules = modules::load_all_modules(config_dir, &cache_base, &[], printer)?;

    let module = match all_modules.get(mod_name) {
        Some(m) => m,
        None => {
            printer.emit(build_module_status_not_found_doc(mod_name));
            return Ok(());
        }
    };

    let state = ctx.state()?;
    let state_rec = state.module_state_by_name(mod_name)?;

    let status = state_rec
        .as_ref()
        .map(|s| s.status.clone())
        .unwrap_or_else(|| "not applied".into());
    let last_applied = state_rec.as_ref().map(|s| s.installed_at.clone());

    let deployed_files: Vec<(String, bool)> = state
        .module_deployed_files(mod_name)?
        .into_iter()
        .map(|f| {
            let exists = std::path::Path::new(&f.file_path).exists();
            (f.file_path, exists)
        })
        .collect();

    // Same live, read-only re-check `diff --module` performs, and the same
    // deliberate gate as the profile-wide command: plain `status --module`
    // stays a fast recorded-only dashboard (this module surface has no
    // recorded drift rows of its own to fall back to — module drift is only
    // ever LIVE), and only `--exit-code` pays for a real scan of the file
    // content and installed packages. Without this, a module that was
    // sabotaged out-of-band read as clean forever, because "Deployed Files"
    // below only checks presence.
    let mut drift: Vec<cfgd_core::state::DriftEvent> = Vec::new();
    if exit_code {
        let platform = Platform::current();
        // Deliberately the config-FREE registry: a module resolves against the
        // managers it declares and cannot reach the profile's `packages.custom`,
        // so resolving it through a config-aware registry would map a module
        // package onto a manager the module cannot use.
        let registry = ctx.base_registry();
        let mgr_map = registry.manager_map();
        let resolved_modules = modules::resolve_modules(
            &[mod_name.to_string()],
            config_dir,
            &cache_base,
            &[],
            platform,
            &mgr_map,
            printer,
        )?;
        let resolved = empty_resolved_profile(&[mod_name.to_string()], &ctx.active_profile_name());
        let fm = CfgdFileManager::new(config_dir, &resolved)?;
        for r in super::live_drift::module_file_verify_results(
            &fm,
            config_dir,
            &resolved,
            &resolved_modules,
        )?
        .into_iter()
        .filter(|r| !r.matches)
        {
            drift.push(super::live_drift::drift_event_from(&r));
        }

        let pkg_cx = cfgd_core::providers::PackageContext::new(printer, state);
        for resolved_module in &resolved_modules {
            for pkg in &resolved_module.packages {
                if let Some(pd) = super::diff::package_missing_drift(pkg, &mgr_map, &pkg_cx) {
                    drift.push(super::live_drift::drift_event_from(
                        &cfgd_core::reconciler::VerifyResult {
                            resource_type: "package".to_string(),
                            resource_id: super::diff::package_resource_id(
                                &pd.manager,
                                &pd.packages,
                            ),
                            matches: false,
                            expected: "installed".to_string(),
                            actual: "missing".to_string(),
                        },
                    ));
                }
            }
        }
    }

    let output = ModuleStatus {
        name: mod_name.to_string(),
        packages: module.spec.packages.len(),
        files: module.spec.files.len(),
        depends: module.spec.depends.clone(),
        status,
        last_applied,
        drift_checked_live: exit_code,
        drift,
    };

    printer.emit(build_module_status_doc(&output, &deployed_files));

    if exit_code && !output.drift.is_empty() {
        cfgd_core::exit::ExitCode::DriftDetected.exit();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cfgd_core::output::Printer;
    use cfgd_core::output::Verbosity;
    use cfgd_core::state::{ApplyRecord, ApplyStatus};

    /// The `cfgd status -o json` surface must emit the unified camelCase status
    /// token at `.lastApply.status`. InProgress is the variant where the apply/
    /// status/log spellings historically drifted (`InProgress`/`in_progress`/
    /// `inProgress`); this pins the JSON path to `display_str`.
    #[test]
    fn status_json_last_apply_status_is_camelcase_token() {
        let output = StatusOutput {
            last_apply: Some(ApplyRecord {
                id: 1,
                timestamp: "2026-01-02T03:04:05Z".to_string(),
                profile: "default".to_string(),
                plan_hash: "deadbeef".to_string(),
                status: ApplyStatus::InProgress,
                summary: Some("running".to_string()),
            }),
            drift: Vec::new(),
            sources: Vec::new(),
            pending_decisions: Vec::new(),
            modules: Vec::new(),
            managed_resources: Vec::new(),
            warnings: Vec::new(),
            classification_degraded: false,
            classification_degraded_code: None,
            classification_degraded_reason: None,
            drift_checked_live: false,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["lastApply"]["status"], serde_json::json!("inProgress"));
        assert_eq!(
            json["lastApply"]["status"],
            serde_json::json!(ApplyStatus::InProgress.display_str())
        );
        assert_eq!(json["classificationDegraded"], serde_json::json!(false));
        assert!(
            json.get("classificationDegradedCode").is_none()
                && json.get("classificationDegradedReason").is_none(),
            "a clean payload carries no code or reason field"
        );
    }

    /// The module health line's units agree with their own counts: a module
    /// with one of each reads `1 pkg, 1 file`, and anything else — including
    /// zero — keeps the plural.
    #[test]
    fn module_status_line_units_agree_with_their_counts() {
        let output = StatusOutput {
            last_apply: None,
            drift: Vec::new(),
            sources: Vec::new(),
            pending_decisions: Vec::new(),
            modules: vec![
                ModuleStatusEntry {
                    name: "tmux".to_string(),
                    packages: 1,
                    files: 1,
                    status: "installed".to_string(),
                },
                ModuleStatusEntry {
                    name: "nvim".to_string(),
                    packages: 3,
                    files: 12,
                    status: "installed".to_string(),
                },
                ModuleStatusEntry {
                    name: "git".to_string(),
                    packages: 0,
                    files: 0,
                    status: "installed".to_string(),
                },
            ],
            managed_resources: Vec::new(),
            warnings: Vec::new(),
            classification_degraded: false,
            classification_degraded_code: None,
            classification_degraded_reason: None,
            drift_checked_live: false,
        };

        let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
        printer.emit(build_fleet_status_doc(
            &output,
            &[],
            std::path::Path::new("/etc/cfgd/cfgd.yaml"),
            "default",
        ));
        drop(printer);
        let out = cfgd_core::test_helpers::captured_text(&buf);

        assert!(
            out.contains("1 pkg, 1 file,"),
            "a single package and file must read singular: {out}"
        );
        assert!(
            out.contains("3 pkgs, 12 files,"),
            "many must stay plural: {out}"
        );
        assert!(
            out.contains("0 pkgs, 0 files,"),
            "zero keeps the plural: {out}"
        );
    }

    /// A degraded classification must be visible IN the `-o json` payload:
    /// the human warning is suppressed under structured output, so without
    /// these fields a broken classification is indistinguishable from a clean
    /// machine with nothing pending.
    #[test]
    fn status_json_degraded_classification_is_structural() {
        let output = StatusOutput {
            last_apply: None,
            drift: Vec::new(),
            sources: Vec::new(),
            pending_decisions: Vec::new(),
            modules: Vec::new(),
            managed_resources: Vec::new(),
            warnings: Vec::new(),
            classification_degraded: true,
            classification_degraded_code: Some(
                crate::cli::output_types::ClassificationDegradedCode::SourceUnreadable,
            ),
            classification_degraded_reason: Some(
                "source 'acme': cached config is unreadable".to_string(),
            ),
            drift_checked_live: false,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["classificationDegraded"], serde_json::json!(true));
        assert_eq!(
            json["classificationDegradedCode"],
            serde_json::json!("sourceUnreadable"),
            "the code is the closed, camelCase machine token"
        );
        assert_eq!(
            json["classificationDegradedReason"],
            serde_json::json!("source 'acme': cached config is unreadable")
        );
    }

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
            config_explicit: false,
            profile: None,
            verbose: 0,
            quiet: true,
            no_color: true,
            color: crate::cli::ColorWhen::Auto,
            output: OutputFormatArg(cfgd_core::output::OutputFormat::Table),
            list_envelope: false,
            jsonpath: None,
            state_dir: Some(state_dir.to_path_buf()),
            config_dir: None,
            cache_dir: None,
            runtime_dir: None,
            scope_arg: crate::cli::ScopeArg::User,
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

        let output = cfgd_core::test_helpers::captured_text(&buf);
        assert!(
            output.contains("Status"),
            "should render Status heading, got: {output}"
        );
        assert!(
            output.contains("No applies recorded yet"),
            "empty applies state should render info line, got: {output}"
        );
        assert!(
            output.contains("No drift recorded"),
            "an empty recorded dashboard says what it read, got: {output}"
        );
    }

    #[test]
    fn cmd_status_with_apply_record_prints_last_apply_block() {
        let (_cfg_dir, state_dir, config_path) = setup_env();
        let store = open_state_store(Some(state_dir.path()), cfgd_core::Scope::User).unwrap();
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

        let output = cfgd_core::test_helpers::captured_text(&buf);
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
        let store = open_state_store(Some(state_dir.path()), cfgd_core::Scope::User).unwrap();
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

        let output = cfgd_core::test_helpers::captured_text(&buf);
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
        let store = open_state_store(Some(state_dir.path()), cfgd_core::Scope::User).unwrap();
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

        let output = cfgd_core::test_helpers::captured_text(&buf);
        // The label is appended only when source != "local", and it carries the
        // owner token so the attribution reads the same here as it does over a
        // `cfgd sync` group.
        assert!(
            output.contains("source:team-config"),
            "non-local drift should carry the source owner token, got: {output}"
        );
    }

    #[test]
    fn cmd_status_managed_resources_renders_table() {
        let (_cfg_dir, state_dir, config_path) = setup_env();
        let store = open_state_store(Some(state_dir.path()), cfgd_core::Scope::User).unwrap();
        store
            .upsert_managed_resource("file", "/etc/managed.conf", "local", Some("hashval"), None)
            .unwrap();

        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, buf) = test_printers();

        cmd_status(&cli, &printer, None, false).unwrap();
        drop(printer);

        let output = cfgd_core::test_helpers::captured_text(&buf);
        assert!(
            output.contains("Managed Resources"),
            "should print Managed Resources section, got: {output}"
        );
        assert!(
            output.contains("/etc/managed.conf"),
            "managed resource row should be present, got: {output}"
        );
    }

    // onChange scripts persist under resource_type
    // "Running script" (execute_script's own return value), distinct from
    // the main pre/post-apply phase scripts' "script" type
    // (apply_script_action's return value). Both must condense for human
    // display; the stored/JSON id must stay the raw multi-line body.
    #[test]
    fn cmd_status_running_script_managed_resource_condenses_for_human_display() {
        let (_cfg_dir, state_dir, config_path) = setup_env();
        let store = open_state_store(Some(state_dir.path()), cfgd_core::Scope::User).unwrap();
        let raw_body = " echo one\necho two\necho three";
        store
            .upsert_managed_resource("Running script", raw_body, "local", None, None)
            .unwrap();

        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, buf) = test_printers();

        cmd_status(&cli, &printer, None, false).unwrap();
        drop(printer);

        let output = cfgd_core::test_helpers::captured_text(&buf);
        assert!(
            !output.contains("echo two"),
            "human table cell must not leak the raw multi-line body: {output}"
        );
        assert!(
            output.contains("echo one"),
            "condensed label should reference the first line: {output}"
        );
    }

    #[test]
    fn cmd_status_running_script_json_preserves_raw_resource_id() {
        let (_cfg_dir, state_dir, config_path) = setup_env();
        let store = open_state_store(Some(state_dir.path()), cfgd_core::Scope::User).unwrap();
        let raw_body = " echo one\necho two\necho three";
        store
            .upsert_managed_resource("Running script", raw_body, "local", None, None)
            .unwrap();

        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, buf) = test_printers_json();

        cmd_status(&cli, &printer, None, false).unwrap();
        drop(printer);

        let output = cfgd_core::test_helpers::captured_text(&buf);
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        let resources = parsed["managedResources"].as_array().unwrap();
        assert_eq!(
            resources[0]["resourceId"], raw_body,
            "JSON payload must preserve the raw multi-line resource_id byte-identical, got: {output}"
        );
    }

    #[test]
    fn cmd_status_exit_code_false_with_drift_returns_ok() {
        // Guard: when --exit-code is not set, drift presence must NOT trigger
        // process::exit. Only the non-exiting half is testable in-process; the
        // drift-present branch would terminate the test runner via process::exit.
        let (_cfg_dir, state_dir, config_path) = setup_env();
        let store = open_state_store(Some(state_dir.path()), cfgd_core::Scope::User).unwrap();
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
        let store = open_state_store(Some(state_dir.path()), cfgd_core::Scope::User).unwrap();
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

        let captured = cfgd_core::test_helpers::captured_text(&buf);
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

        let output = cfgd_core::test_helpers::captured_text(&buf);
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

        cmd_status_module(&RunContext::new(&cli, &printer), "ghost", false).unwrap();
        drop(printer);

        let output = cfgd_core::test_helpers::captured_text(&buf);
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

        cmd_status_module(&RunContext::new(&cli, &printer), "ghost", false).unwrap();
        drop(printer);

        let captured = cfgd_core::test_helpers::captured_text(&buf);
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
        let store = open_state_store(Some(state_dir.path()), cfgd_core::Scope::User).unwrap();
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

        cmd_status_module(&RunContext::new(&cli, &printer), "test-mod", false).unwrap();
        drop(printer);

        let output = cfgd_core::test_helpers::captured_text(&buf);
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

        cmd_status_module(&RunContext::new(&cli, &printer), "test-mod", false).unwrap();
        drop(printer);

        let output = cfgd_core::test_helpers::captured_text(&buf);
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

        let store = open_state_store(Some(state_dir.path()), cfgd_core::Scope::User).unwrap();
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

        cmd_status_module(&RunContext::new(&cli, &printer), "test-mod", false).unwrap();
        drop(printer);

        let output = cfgd_core::test_helpers::captured_text(&buf);
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

        let store = open_state_store(Some(state_dir.path()), cfgd_core::Scope::User).unwrap();
        store
            .upsert_module_state("test-mod", None, "pkgh", "fileh", None, "installed")
            .unwrap();

        let mut cli = test_cli_for(config_path, state_dir.path());
        cli.output = OutputFormatArg(cfgd_core::output::OutputFormat::Json);
        let (printer, buf) = test_printers_json();

        cmd_status_module(&RunContext::new(&cli, &printer), "test-mod", false).unwrap();
        drop(printer);

        let captured = cfgd_core::test_helpers::captured_text(&buf);
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

    // The drift-catching, exit(5) branch is proven by the real subprocess in
    // `tests/cli_integration.rs::status_module_exit_code_catches_module_file_drift`
    // — `process::exit` cannot be exercised in-process. This test proves the
    // complementary path: a converged module's live scan finds nothing, so
    // `--exit-code` must return Ok rather than calling `process::exit`.
    #[test]
    fn cmd_status_module_exit_code_true_no_drift_returns_ok() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let config_dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();
        let target = target_dir.path().join("converged.conf");
        std::fs::write(&target, "same content\n").unwrap();

        let config_path = config_dir.path().join("cfgd.yaml");
        std::fs::write(&config_path, CONFIG_YAML).unwrap();
        let profiles_dir = config_dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(profiles_dir.join("default.yaml"), PROFILE_WITH_MODULE_YAML).unwrap();
        let mod_dir = config_dir.path().join("modules").join("test-mod");
        std::fs::create_dir_all(&mod_dir).unwrap();
        std::fs::write(mod_dir.join("conf"), "same content\n").unwrap();
        let module_yaml = format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-mod\nspec:\n  packages: []\n  files:\n    - source: conf\n      target: {}\n",
            cfgd_core::to_posix_string(&target)
        );
        std::fs::write(mod_dir.join("module.yaml"), module_yaml).unwrap();

        let mut cli = test_cli_for(config_path, state_dir.path());
        cli.output = OutputFormatArg(cfgd_core::output::OutputFormat::Json);
        let (printer, buf) = test_printers_json();

        let res = cmd_status_module(&RunContext::new(&cli, &printer), "test-mod", true);
        assert!(
            res.is_ok(),
            "exit_code=true with a converged module must return Ok, got: {res:?}"
        );
        drop(printer);

        let captured = cfgd_core::test_helpers::captured_text(&buf);
        let parsed: serde_json::Value = serde_json::from_str(captured.trim())
            .unwrap_or_else(|e| panic!("invalid JSON: {e}, got: {captured}"));
        assert_eq!(
            parsed["driftCheckedLive"], true,
            "exit_code=true must have actually run the live scan, got: {parsed}"
        );
        assert_eq!(
            parsed["drift"],
            serde_json::json!([]),
            "a converged module's live scan must find nothing, got: {parsed}"
        );
    }
}
