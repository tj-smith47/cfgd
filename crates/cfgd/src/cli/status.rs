use super::*;
use cfgd_core::PathDisplayExt;
use cfgd_core::config::LOCAL_LAYER;
use cfgd_core::output::{Doc, OwnerLabel, Printer, Role, condense_script_label, renderer::Table};

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
    /// empty however far the machine has drifted; only `--scan` (and
    /// `--exit-code`, which implies it) scans.
    /// A consumer differencing an empty list needs to know which of those two
    /// it is holding, and the human line says the same thing in words.
    pub drift_checked_live: bool,
    /// When this machine was last scanned for live drift (`--scan`,
    /// `--exit-code`, `diff`, `verify`, or a daemon reconcile tick) — `None`
    /// when it never has been.
    ///
    /// A scanning run reports its OWN scan here, so the field always describes
    /// the `drift` array beside it. The recorded-state header's age line is
    /// computed from the value read BEFORE any scan, because that line exists
    /// to date state the run did NOT check — and it renders only on the
    /// non-scanning branch, where the two values are the same.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_scan_at: Option<String>,
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
    /// Counts of the declared surfaces apply runs a phase for but that carry
    /// no per-item recorded state: a phase that ran with nothing to say about
    /// it in status is a phase the reader watched happen and then could not
    /// find. `cfgd module show` itemizes what these summarize.
    pub env: usize,
    pub aliases: usize,
    /// Lifecycle hooks the module declares, by name (`preApply`, `onDrift`, …).
    pub scripts: Vec<String>,
    /// System configurators the module contributes settings to, by name.
    pub system: Vec<String>,
    pub depends: Vec<String>,
    pub status: String,
    pub last_applied: Option<String>,
    /// One row per DECLARED package, carrying what the machine holds — the
    /// state half of the count above. Every row reads `notScanned` unless
    /// `--scan` asked a manager.
    pub package_state: Vec<ModulePackageStatus>,
    /// One row per file this module has deployed, carrying the same verdict
    /// the drift scan reached. Never a bare presence check: a drifted file is
    /// present, and reporting presence as health is the contradiction this
    /// field exists to make unrepresentable.
    pub deployed_files: Vec<ModuleFileStatus>,
    /// Live drift found for this module's files and packages. Always empty
    /// unless `--scan` (or `--exit-code`, which implies it) requested the live
    /// scan — see `drift_checked_live`.
    pub drift: Vec<cfgd_core::state::DriftEvent>,
    /// Whether `drift` is the verdict of a live scan of this module or just
    /// an unchecked empty default. Mirrors `StatusOutput::drift_checked_live`
    /// so the two `-o json` shapes read the same way.
    pub drift_checked_live: bool,
}

/// What a manager reports about one declared package.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum ModulePackagePresence {
    Installed,
    NotInstalled,
    /// Nothing asked: no `--scan`, a `script` package with no manager to ask,
    /// or a manager this host does not have registered.
    NotScanned,
    /// The module's own `platforms` gate rules this package out on this host,
    /// so nothing was ever going to install it. Distinct from `NotScanned`,
    /// which says nobody looked: here the answer is known and `cfgd module
    /// show` renders the same words for the same package.
    PlatformSkipped,
}

impl ModulePackagePresence {
    fn role(self) -> Role {
        match self {
            Self::Installed => Role::Ok,
            Self::NotInstalled => Role::Warn,
            Self::NotScanned | Self::PlatformSkipped => Role::Info,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::NotInstalled => cfgd_core::Absence::NotInstalled.as_str(),
            Self::NotScanned => NOT_SCANNED,
            Self::PlatformSkipped => PLATFORM_SKIPPED,
        }
    }
}

/// What this run can say about one file the module deployed.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum ModuleFilePresence {
    Deployed,
    /// Present, and its content is not what the module declares — the same
    /// verdict the Drift section reports for it, so the two can never disagree.
    Drifted,
    Missing,
    /// Present on disk, content unchecked (no `--scan`). Presence alone is not
    /// health.
    NotScanned,
}

impl ModuleFilePresence {
    fn role(self) -> Role {
        match self {
            Self::Deployed => Role::Ok,
            Self::Drifted => Role::Warn,
            Self::Missing => Role::Fail,
            Self::NotScanned => Role::Info,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Deployed => "deployed",
            Self::Drifted => "drifted",
            Self::Missing => cfgd_core::Absence::Missing.as_str(),
            Self::NotScanned => NOT_SCANNED,
        }
    }
}

/// The one spelling of "cfgd did not ask", shared by both state vocabularies
/// so a reader meets one phrase per report rather than one per section.
const NOT_SCANNED: &str = "not scanned";

/// The wording `cfgd module show` renders for a platform-gated package
/// (`module/list_show.rs`); the two surfaces answer about one declared package
/// and must say the same thing.
pub(in crate::cli) const PLATFORM_SKIPPED: &str = "skipped (platform filter)";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModulePackageStatus {
    pub name: String,
    /// The manager that answered. `None` when nothing asked, so the row can
    /// never name a manager as the authority for a verdict it did not give.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manager: Option<String>,
    pub state: ModulePackagePresence,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleFileStatus {
    pub path: String,
    pub state: ModuleFilePresence,
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
        doc.section("Drift", |s| {
            if checked_live {
                s.status(Role::Ok, "No drift detected")
            } else {
                s.status_with(Role::Ok, "No drift recorded", |sf| {
                    sf.detail("`cfgd diff` checks the live machine")
                })
            }
        })
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
                    let label_text = OwnerLabel::new("source", &event.source).plain();
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

/// The recorded-state header's staleness threshold: a daemon's default
/// reconcile interval. Past this age, the recorded drift a plain `cfgd status`
/// shows could easily be older than a live daemon would ever let it get, so
/// the header hints at `--scan` instead of leaving the reader to guess.
const SCAN_STALENESS_SECS: i64 = cfgd_core::daemon::DEFAULT_RECONCILE_SECS as i64;

/// Build the fleet-wide `cfgd status` Doc. Caller supplies the precomputed
/// payload and the configured `SourceSpec` list so the renderer can show
/// "not yet fetched" rows for sources without state records.
pub fn build_fleet_status_doc(
    output: &StatusOutput,
    configured_sources: &[String],
    config_path: &Path,
    profile_name: &str,
    now: &str,
) -> Doc {
    let mut doc = Doc::new()
        .heading("Status")
        .kv("Config", config_path.display_posix())
        .kv("Profile", profile_name);

    // Only the recorded-state dashboard needs a staleness signal: a `--scan`/
    // `--exit-code` run just checked the machine itself, so its Drift section
    // already speaks for how current the display is.
    if !output.drift_checked_live {
        match &output.last_scan_at {
            Some(ts) => {
                let age = cfgd_core::humanize_age_since(ts, now).unwrap_or_else(|| ts.clone());
                doc = doc.kv("Last Scan", &age);
                if cfgd_core::is_stale_since(ts, now, SCAN_STALENESS_SECS) {
                    doc = doc.hint("Run `cfgd status --scan` for a live check");
                }
            }
            None => {
                doc = doc
                    .kv("Last Scan", "never")
                    .hint("Run `cfgd status --scan` for a live check");
            }
        }
    }

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
            s.status_with(role, OwnerLabel::new("module", &m.name).plain(), |f| {
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
///
/// Every row's subject is the thing's identity and its detail is what the
/// machine holds — the same grammar the fleet doc's module rows read in, so
/// one report never states a fact the other contradicts.
pub fn build_module_status_doc(output: &ModuleStatus) -> Doc {
    let mut doc = Doc::new()
        .heading_title("Status", &output.name)
        .kv("Packages", output.packages.to_string())
        .kv("Files", output.files.to_string());

    if output.env > 0 {
        doc = doc.kv("Env", output.env.to_string());
    }
    if output.aliases > 0 {
        doc = doc.kv("Aliases", output.aliases.to_string());
    }
    if !output.scripts.is_empty() {
        doc = doc.kv("Scripts", output.scripts.join(", "));
    }
    if !output.system.is_empty() {
        doc = doc.kv("System", output.system.join(", "));
    }
    if !output.depends.is_empty() {
        doc = doc.kv("Dependencies", output.depends.join(", "));
    }

    doc = doc.kv("Status", &output.status);
    if let Some(last) = &output.last_applied {
        doc = doc.kv("Last applied", last);
    }

    doc = render_drift_section(doc, &output.drift, output.drift_checked_live);

    doc = doc.section_if_nonempty("Packages", &output.package_state, |s, pkgs| {
        pkgs.iter().fold(s, |s, pkg| {
            // The manager rides in the detail beside the verdict it gave,
            // never in the subject: the subject is the name the user declared,
            // and an unscanned row has no manager to name.
            let detail = match &pkg.manager {
                Some(m) => format!("{} ({m})", pkg.state.label()),
                None => pkg.state.label().to_string(),
            };
            s.status_with(pkg.state.role(), &pkg.name, |f| f.detail(detail))
        })
    });

    doc = doc.section_if_nonempty("Deployed Files", &output.deployed_files, |s, files| {
        files.iter().fold(s, |s, file| {
            s.status_with(file.state.role(), &file.path, |f| {
                f.detail(file.state.label())
            })
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
        env: 0,
        aliases: 0,
        scripts: Vec::new(),
        system: Vec::new(),
        depends: Vec::new(),
        status: "not found".into(),
        last_applied: None,
        package_state: Vec::new(),
        deployed_files: Vec::new(),
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
    scan: bool,
) -> anyhow::Result<()> {
    // `--exit-code` implies the live scan `--scan` names explicitly: a CI
    // gate has to reflect reality regardless of whether the caller also asked
    // to see it. `exit_code` alone still decides whether the run EXITS
    // nonzero on drift — `--scan` on its own never changes the exit code.
    let do_scan = exit_code || scan;
    let ctx = RunContext::new(cli, printer);
    if let Some(mod_name) = module_filter {
        return cmd_status_module(&ctx, mod_name, exit_code, do_scan);
    }

    let (cfg, profile_name, local_resolved) = ctx.config_and_profile()?;
    let state = ctx.state()?;

    let last_apply = state.last_apply()?;
    // Read before this run's own scan (if any) overwrites it: the header's
    // staleness signal is about what the RECORDED state was last checked
    // against, not about the scan this very invocation is about to perform.
    let last_scan_at = state.last_scan_at()?;
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
    // Taken ONLY for the live scan below, the one half that reads a registry: a
    // plain `cfgd status` is an offline dashboard, and building a registry it
    // never reads would construct every package manager and configurator the
    // host supports for nothing. Taken here rather than at the scan because the
    // two field moves below are partial moves out of `desired`, which block
    // the `&mut self` the accessor needs — and `Some` exactly when `do_scan`,
    // so the scan below can bind it instead of re-testing the flag.
    let registry = do_scan.then(|| desired.take_registry(cfg));
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
        drift_checked_live: do_scan,
        last_scan_at,
    };

    // Plain `status` (no --scan/--exit-code) keeps the fast RECORDED-drift
    // dashboard by deliberate design. `--scan` (and `--exit-code`, which
    // implies it), however, must reflect REALITY: a host with no daemon and no
    // prior scan has zero recorded events even when a managed file was just
    // edited out-of-band. So when scanning, run the LIVE, read-only scan
    // (never recording drift rows — the same checks `diff`/`verify` run, though
    // it DOES stamp the last-scan timestamp this header reads back next time)
    // BEFORE emitting, fold its findings into the displayed Drift section, then
    // exit 5 if `--exit-code` asked for it and any drift was found. This keeps
    // the human verdict and the exit code in agreement instead of printing "No
    // drift detected" alongside exit 5.
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
            state,
            &pkg_cx,
        )?;
        // The payload's `lastScanAt` must describe the scan that PRODUCED it,
        // or a consumer pairing it with `driftCheckedLive: true` reads
        // "scanned live, last scanned two hours ago". A refused write leaves
        // the pre-scan value read above standing, so the field never names a
        // stamp the store does not hold. The header row read that same value
        // and is not rendered on this branch anyway.
        if let Some(stamped) = state.record_scan() {
            output.last_scan_at = Some(stamped);
        }
        for r in &drift {
            output.drift.push(super::live_drift::drift_event_from(
                r,
                &resolved.merged.env,
                &resolved.merged.aliases,
            ));
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
        &cfgd_core::utc_now_iso8601(),
    ));

    if exit_code && !live_drift.is_empty() {
        cfgd_core::exit::ExitCode::DriftDetected.exit();
    }

    Ok(())
}

/// The lifecycle hooks a module declares, by the name the YAML spells them
/// with. Apply opens a phase for each one that has entries, so each has to be
/// findable in the module's report.
fn declared_script_hooks(spec: Option<&cfgd_core::config::ScriptSpec>) -> Vec<String> {
    let Some(spec) = spec else {
        return Vec::new();
    };
    [
        ("preApply", &spec.pre_apply),
        ("postApply", &spec.post_apply),
        ("preReconcile", &spec.pre_reconcile),
        ("postReconcile", &spec.post_reconcile),
        ("onDrift", &spec.on_drift),
        ("onChange", &spec.on_change),
    ]
    .into_iter()
    .filter(|(_, entries)| !entries.is_empty())
    .map(|(name, _)| name.to_string())
    .collect()
}

/// Pair each DECLARED package with the scan verdict resolution produced for it.
///
/// The two lists are joined by name and by ORDER, never by name alone: one name
/// may be declared twice under two managers (the `brew` / `brew-cask` shape),
/// and a single slot per name kept only the last verdict and rendered both rows
/// as that one manager. A gated entry is answered before the queue is drawn
/// from — it produced no resolution, so consuming one would hand it the verdict
/// belonging to its same-named sibling.
fn join_package_state(
    declared: &[cfgd_core::config::ModulePackageEntry],
    scanned: &mut std::collections::HashMap<
        String,
        std::collections::VecDeque<(String, ModulePackagePresence)>,
    >,
    here: &Platform,
) -> Vec<ModulePackageStatus> {
    declared
        .iter()
        .map(|p| {
            if !here.matches_any(&p.platforms) {
                return ModulePackageStatus {
                    name: p.name.clone(),
                    manager: None,
                    state: ModulePackagePresence::PlatformSkipped,
                };
            }
            match scanned
                .get_mut(&p.name)
                .and_then(std::collections::VecDeque::pop_front)
            {
                Some((manager, state)) => ModulePackageStatus {
                    name: p.name.clone(),
                    manager: Some(manager),
                    state,
                },
                None => ModulePackageStatus {
                    name: p.name.clone(),
                    manager: None,
                    state: ModulePackagePresence::NotScanned,
                },
            }
        })
        .collect()
}

pub(super) fn cmd_status_module(
    ctx: &RunContext<'_>,
    mod_name: &str,
    exit_code: bool,
    do_scan: bool,
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

    // Same live, read-only re-check `diff --module` performs, and the same
    // deliberate gate as the profile-wide command: plain `status --module`
    // stays a fast recorded-only dashboard (this module surface has no
    // recorded drift rows of its own to fall back to — module drift is only
    // ever LIVE), and only `--scan`/`--exit-code` (which implies `--scan`)
    // pays for a real scan of the file content and installed packages.
    // Without this, a module that was sabotaged out-of-band read as clean
    // forever, because "Deployed Files" below only checks presence.
    // Deliberately no `record_scan` below, unlike the fleet-wide path and the
    // sibling scans in `diff`/`verify`: the stamp dates the FLEET-wide
    // dashboard's header, and one module's files and packages are not evidence
    // the machine was checked.
    let mut drift: Vec<cfgd_core::state::DriftEvent> = Vec::new();
    // The verify ids of the files this scan found drifted. The Deployed Files
    // rows are judged against it, so the two sections state one verdict per
    // file instead of a content check and a presence check disagreeing.
    let mut drifted_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Keyed by DECLARED name, so the rows below can be built from the declared
    // list and the two can never differ in length: a package resolution
    // dropped (a platform gate) is a package nothing asked about, not a
    // package that vanished from the report.
    // One name may be declared TWICE under two managers (the `brew` /
    // `brew-cask` shape), so each key holds a QUEUE in resolution order and
    // each declared row consumes its own verdict. A single slot per name kept
    // only the last, and rendered both rows as that one manager.
    let mut scanned_packages: std::collections::HashMap<
        String,
        std::collections::VecDeque<(String, ModulePackagePresence)>,
    > = std::collections::HashMap::new();
    if do_scan {
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
        // One spinner across this module's live scan, narrated per pass.
        printer.narrate(
            format!("Scanning module:{mod_name} files"),
            |sp| -> anyhow::Result<()> {
                let file_results = super::live_drift::module_file_verify_results(
                    &fm,
                    config_dir,
                    &resolved,
                    &resolved_modules,
                )?;
                for r in file_results.into_iter().filter(|r| !r.matches) {
                    drifted_ids.insert(r.resource_id.clone());
                    drift.push(super::live_drift::drift_event_from(
                        &r,
                        &resolved.merged.env,
                        &resolved.merged.aliases,
                    ));
                }

                sp.set_message(format!("Scanning module:{mod_name} packages"));
                // ONE context across every package of every resolved module,
                // so a manager is enumerated once however many packages name
                // it (`PackageContext::installed_for`'s memo).
                let pkg_cx = cfgd_core::providers::PackageContext::new(printer, state);
                for resolved_module in &resolved_modules {
                    for pkg in &resolved_module.packages {
                        // A `script` package and a manager this host has not
                        // registered are both questions nothing can answer —
                        // `package_missing_drift` returns `None` for each, and
                        // reading that as "installed" would report a verdict
                        // no manager gave.
                        let presence = if pkg.manager == "script"
                            || !mgr_map.contains_key(pkg.manager.as_str())
                        {
                            ModulePackagePresence::NotScanned
                        } else if let Some(pd) =
                            super::diff::package_missing_drift(pkg, &mgr_map, &pkg_cx)
                        {
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
                                &resolved.merged.env,
                                &resolved.merged.aliases,
                            ));
                            ModulePackagePresence::NotInstalled
                        } else {
                            ModulePackagePresence::Installed
                        };
                        // Drift is collected for the dependency modules this
                        // resolution pulled in too (they are why the named
                        // module works); the package ROWS report the module
                        // the reader asked about, whose declared count heads
                        // the report.
                        if resolved_module.name == mod_name {
                            scanned_packages
                                .entry(pkg.canonical_name.clone())
                                .or_default()
                                .push_back((pkg.manager.clone(), presence));
                        }
                    }
                }
                Ok(())
            },
        )?;
    }

    let package_state = join_package_state(
        &module.spec.packages,
        &mut scanned_packages,
        cfgd_core::platform::Platform::current(),
    );

    let deployed_files: Vec<ModuleFileStatus> = state
        .module_deployed_files(mod_name)?
        .into_iter()
        .map(|f| {
            // Absence is definite whether or not a scan ran; presence is not.
            // Without a live check the honest verdict on a file that is THERE
            // is that nothing looked inside it — `Path::exists` cannot tell a
            // converged file from a tampered one.
            let state = if !std::path::Path::new(&f.file_path).exists() {
                ModuleFilePresence::Missing
            } else if !do_scan {
                ModuleFilePresence::NotScanned
            } else if drifted_ids.contains(&super::live_drift::module_file_resource_id(
                mod_name,
                &f.file_path,
            )) {
                ModuleFilePresence::Drifted
            } else {
                ModuleFilePresence::Deployed
            };
            ModuleFileStatus {
                path: f.file_path,
                state,
            }
        })
        .collect();

    let output = ModuleStatus {
        name: mod_name.to_string(),
        packages: module.spec.packages.len(),
        files: module.spec.files.len(),
        env: module.spec.env.len(),
        aliases: module.spec.aliases.len(),
        scripts: declared_script_hooks(module.spec.scripts.as_ref()),
        system: module.spec.system.keys().cloned().collect(),
        depends: module.spec.depends.clone(),
        status,
        last_applied,
        package_state,
        deployed_files,
        drift_checked_live: do_scan,
        drift,
    };

    printer.emit(build_module_status_doc(&output));

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
            last_scan_at: None,
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
            last_scan_at: None,
        };

        let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
        printer.emit(build_fleet_status_doc(
            &output,
            &[],
            std::path::Path::new("/etc/cfgd/cfgd.yaml"),
            "default",
            "2026-05-12T14:30:25Z",
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

    /// The recorded-state header says when the shown state was last checked
    /// against the machine, and hints at `--scan` once that answer is old
    /// enough to be misleading. The threshold is the daemon's default
    /// reconcile interval: past it, the dashboard is showing something a live
    /// daemon would never have let get this stale.
    ///
    /// A run that DID scan says nothing here — its Drift section already
    /// speaks for how current the display is — which is the branch that keeps
    /// `--scan`'s own output from carrying a hint pointing back at itself.
    #[test]
    fn status_header_dates_the_recorded_state_and_hints_when_it_is_stale() {
        fn header(last_scan_at: Option<&str>, checked_live: bool) -> String {
            let output = StatusOutput {
                last_apply: None,
                drift: Vec::new(),
                sources: Vec::new(),
                pending_decisions: Vec::new(),
                modules: Vec::new(),
                managed_resources: Vec::new(),
                warnings: Vec::new(),
                classification_degraded: false,
                classification_degraded_code: None,
                classification_degraded_reason: None,
                drift_checked_live: checked_live,
                last_scan_at: last_scan_at.map(str::to_string),
            };
            let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
            printer.emit(build_fleet_status_doc(
                &output,
                &[],
                std::path::Path::new("/etc/cfgd/cfgd.yaml"),
                "default",
                // Pinned, never the wall clock: the age is a rendered value.
                "2026-05-14T10:05:00Z",
            ));
            drop(printer);
            cfgd_core::test_helpers::captured_text(&buf)
        }

        let hint = "cfgd status --scan";

        // Exactly at the threshold is not yet stale — `is_stale_since` is
        // "more than", so the boundary belongs to the fresh side and a daemon
        // reconciling on schedule never trips the hint.
        let fresh = header(Some("2026-05-14T10:00:00Z"), false);
        assert!(fresh.contains("Last Scan"), "no age row: {fresh}");
        assert!(fresh.contains("5m ago"), "wrong age rendered: {fresh}");
        assert!(!fresh.contains(hint), "a fresh scan must not hint: {fresh}");

        let stale = header(Some("2026-05-14T08:00:00Z"), false);
        assert!(stale.contains("2h ago"), "wrong age rendered: {stale}");
        assert!(stale.contains(hint), "a stale scan must hint: {stale}");

        let never = header(None, false);
        assert!(never.contains("never"), "no never row: {never}");
        assert!(never.contains(hint), "an unscanned host must hint: {never}");

        let scanned = header(Some("2026-05-14T08:00:00Z"), true);
        assert!(
            !scanned.contains("Last Scan") && !scanned.contains(hint),
            "a run that just scanned must not date or hint at itself: {scanned}"
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
            last_scan_at: None,
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

        let err = cmd_status(&cli, &printer, None, false, false).unwrap_err();
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

        cmd_status(&cli, &printer, None, false, false).unwrap();
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

        cmd_status(&cli, &printer, None, false, false).unwrap();
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

        cmd_status(&cli, &printer, None, false, false).unwrap();
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

        cmd_status(&cli, &printer, None, false, false).unwrap();
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

        cmd_status(&cli, &printer, None, false, false).unwrap();
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

        cmd_status(&cli, &printer, None, false, false).unwrap();
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

        cmd_status(&cli, &printer, None, false, false).unwrap();
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

        let res = cmd_status(&cli, &printer, None, false, false);
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

        let res = cmd_status(&cli, &printer, None, true, true);
        assert!(
            res.is_ok(),
            "exit_code=true with no drift must return Ok, got: {res:?}"
        );
    }

    /// A scan the store refuses to record must leave `lastScanAt` naming the
    /// stamp the row still holds, never the one this run tried to write.
    ///
    /// The store half of that contract is pinned in cfgd-core; the harm lives
    /// here, on the payload: a stamp no row holds reports the machine as
    /// scanned more recently than anything can prove, and the next run that
    /// reads the row sends the dashboard backwards.
    #[test]
    fn cmd_status_scan_keeps_the_recorded_stamp_when_the_write_is_refused() {
        let (_cfg_dir, state_dir, config_path) = setup_env();
        let frozen = "2000-01-01T00:00:00Z";
        {
            let store = open_state_store(Some(state_dir.path()), cfgd_core::Scope::User).unwrap();
            cfgd_core::test_helpers::freeze_last_scan_at(&store, frozen).unwrap();
        }

        let mut cli = test_cli_for(config_path, state_dir.path());
        cli.output = OutputFormatArg(cfgd_core::output::OutputFormat::Json);
        let (printer, buf) = test_printers_json();

        cmd_status(&cli, &printer, None, false, true).unwrap();
        drop(printer);

        let captured = cfgd_core::test_helpers::captured_text(&buf);
        let parsed: serde_json::Value = serde_json::from_str(captured.trim())
            .unwrap_or_else(|e| panic!("invalid JSON: {e}, got: {captured}"));
        assert_eq!(
            parsed["driftCheckedLive"], true,
            "`--scan` ran the live scan, so the payload must say so: {parsed}"
        );
        assert_eq!(
            parsed["lastScanAt"], frozen,
            "a refused write must leave the stored stamp standing: {parsed}"
        );
    }

    /// WARN regression (re-review of the QP13 fix round): `cfgd status
    /// --scan`'s live-drift display must show a drifted env-var/alias's real
    /// declared line, not the opaque `current`/`missing or changed` markers
    /// `verify_env_items` persists. `drift_event_from` (`live_drift.rs`)
    /// shapes every live-scan finding into the exact `StatusOutput.drift`
    /// vec this test reads back from `-o json`, and `render_drift_section`
    /// renders the same `DriftEvent.expected` string to the human terminal —
    /// so recomputing at that one shaping point fixes both surfaces. This is
    /// the sibling of `cmd_diff_reports_no_env_drift_when_bootstrap_path_dirs_are_recorded`'s
    /// fix, on `status`'s parallel live-scan path rather than `diff`'s.
    #[test]
    fn cmd_status_scan_shows_the_declared_env_value_not_the_opaque_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(&config_path, CONFIG_YAML).unwrap();
        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(
            profiles_dir.join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  envScope: Interactive\n  env:\n    - name: EDITOR\n      value: vim\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("modules")).unwrap();

        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        // Header only, no `EDITOR` line — the per-item check reports the
        // declared var as drifted (opaque "missing or changed" before this
        // fix; the real declared line after it).
        std::fs::write(
            tmp_home
                .path()
                .join(crate::cli::helpers::tests::primary_env_file_name()),
            "# managed by cfgd \u{2014} do not edit\n",
        )
        .unwrap();
        // The declared line's dialect is platform-dependent (bash `export`
        // vs PowerShell `$env:`), so the expected needle is derived from
        // `env_item_declared_line` — production's own per-item renderer for
        // the running platform — rather than a hardcoded POSIX literal.
        let declared_env = vec![cfgd_core::config::EnvVar {
            name: "EDITOR".to_string(),
            value: "vim".to_string(),
        }];
        let declared_line =
            cfgd_core::reconciler::env_item_declared_line("env-var", "EDITOR", &declared_env, &[])
                .expect("EDITOR renders a declared line");

        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let mut cli = test_cli_for(config_path, &state_dir);
        cli.output = OutputFormatArg(cfgd_core::output::OutputFormat::Json);
        let (printer, buf) = test_printers_json();

        cmd_status(&cli, &printer, None, false, true).unwrap();
        drop(printer);

        let captured = cfgd_core::test_helpers::captured_text(&buf);
        let parsed: serde_json::Value = serde_json::from_str(captured.trim())
            .unwrap_or_else(|e| panic!("invalid JSON: {e}, got: {captured}"));
        let drift = parsed["drift"].as_array().expect("drift array");
        let editor_row = drift
            .iter()
            .find(|d| d["resourceType"] == "env-var" && d["resourceId"] == "EDITOR")
            .unwrap_or_else(|| panic!("expected an EDITOR env-var drift row: {parsed}"));
        assert_eq!(
            editor_row["expected"],
            serde_json::json!(declared_line),
            "the -o json payload must carry the declared line, not the opaque \
             marker: {editor_row}"
        );
        assert_ne!(
            editor_row["expected"],
            serde_json::json!("current"),
            "must not regress to the opaque marker: {editor_row}"
        );

        // The human render shares the same `DriftEvent`, so the fix must show
        // up there too — assert its content directly rather than trusting the
        // JSON assertion above to stand in for it.
        let (human_printer, human_buf) = test_printers();
        cmd_status(&cli, &human_printer, None, false, true).unwrap();
        drop(human_printer);
        let human = cfgd_core::test_helpers::captured_text(&human_buf);
        let editor_line = human
            .lines()
            .find(|l| l.contains("env-var EDITOR"))
            .unwrap_or_else(|| panic!("expected an EDITOR env-var drift line, got: {human}"));
        assert!(
            editor_line.contains(&declared_line),
            "the human render must show the declared line, got: {editor_line}"
        );
        assert!(
            !editor_line.contains("want: current"),
            "the EDITOR row must not show the opaque marker, got: {editor_line}"
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

        cmd_status(&cli, &printer, None, false, false).unwrap();
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

        cmd_status(&cli, &printer, Some("test-mod"), false, false).unwrap();
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

        cmd_status_module(&RunContext::new(&cli, &printer), "ghost", false, false).unwrap();
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

        cmd_status_module(&RunContext::new(&cli, &printer), "ghost", false, false).unwrap();
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

        cmd_status_module(&RunContext::new(&cli, &printer), "test-mod", false, false).unwrap();
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

    fn declared(name: &str, platforms: &[&str]) -> cfgd_core::config::ModulePackageEntry {
        cfgd_core::config::ModulePackageEntry {
            name: name.to_string(),
            platforms: platforms.iter().map(|p| (*p).to_string()).collect(),
            ..Default::default()
        }
    }

    /// One name declared twice under two managers is two rows with two
    /// verdicts. Keyed by name alone, the second resolution overwrote the
    /// first and both rows rendered the same manager.
    #[test]
    fn two_declarations_of_one_name_each_keep_their_own_manager() {
        let mut scanned = std::collections::HashMap::new();
        scanned.insert(
            "docker".to_string(),
            std::collections::VecDeque::from(vec![
                ("brew".to_string(), ModulePackagePresence::Installed),
                ("brew-cask".to_string(), ModulePackagePresence::NotInstalled),
            ]),
        );

        let rows = join_package_state(
            &[declared("docker", &[]), declared("docker", &[])],
            &mut scanned,
            Platform::current(),
        );

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].manager.as_deref(), Some("brew"));
        assert_eq!(rows[0].state, ModulePackagePresence::Installed);
        assert_eq!(rows[1].manager.as_deref(), Some("brew-cask"));
        assert_eq!(rows[1].state, ModulePackagePresence::NotInstalled);
    }

    /// A gated entry resolved to nothing, so it must not consume the verdict
    /// its same-named sibling earned.
    #[test]
    fn a_gated_declaration_does_not_consume_its_siblings_verdict() {
        let mut scanned = std::collections::HashMap::new();
        scanned.insert(
            "docker".to_string(),
            std::collections::VecDeque::from(vec![(
                "brew".to_string(),
                ModulePackagePresence::Installed,
            )]),
        );

        let rows = join_package_state(
            &[declared("docker", &["plan9"]), declared("docker", &[])],
            &mut scanned,
            Platform::current(),
        );

        assert_eq!(rows[0].state, ModulePackagePresence::PlatformSkipped);
        assert_eq!(rows[0].manager, None);
        assert_eq!(rows[1].manager.as_deref(), Some("brew"));
        assert_eq!(rows[1].state, ModulePackagePresence::Installed);
    }

    /// A package the module's own `platforms` gate rules out is not "not
    /// scanned" — nobody was ever going to look. `cfgd module show` says
    /// `skipped (platform filter)` for the same package, and two surfaces
    /// answering one question differently is the drift this pins.
    #[test]
    fn a_platform_gated_package_reads_skipped_not_unscanned() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let config_dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let config_path = config_dir.path().join("cfgd.yaml");
        std::fs::write(&config_path, CONFIG_YAML).unwrap();
        let profiles_dir = config_dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(profiles_dir.join("default.yaml"), PROFILE_WITH_MODULE_YAML).unwrap();
        let mod_dir = config_dir.path().join("modules").join("test-mod");
        std::fs::create_dir_all(&mod_dir).unwrap();
        // `plan9` is no OS, distro or arch cfgd targets, so the gate closes on
        // every host the suite runs on.
        std::fs::write(
            mod_dir.join("module.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-mod\nspec:\n  packages:\n    - name: ripgrep\n    - name: plan9-only\n      platforms:\n        - plan9\n",
        )
        .unwrap();

        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, buf) = test_printers();
        cmd_status_module(&RunContext::new(&cli, &printer), "test-mod", false, false).unwrap();
        drop(printer);

        let output = cfgd_core::test_helpers::captured_text(&buf);
        let gated = output
            .lines()
            .find(|l| l.contains("plan9-only"))
            .unwrap_or_else(|| panic!("gated package must render a row, got:\n{output}"));
        assert!(
            gated.contains(PLATFORM_SKIPPED),
            "gated package must read the platform-filter wording, got: {gated}"
        );
        let ungated = output
            .lines()
            .find(|l| l.contains("ripgrep"))
            .unwrap_or_else(|| panic!("ungated package must render a row, got:\n{output}"));
        assert!(
            ungated.contains(NOT_SCANNED),
            "an ungated package with no scan still reads not scanned, got: {ungated}"
        );
    }

    /// The whole shape of the bug: a module whose declared packages the machine
    /// already holds plans nothing, so `Reconciler::apply` — the only writer of
    /// `module_state` — never runs. The run must still record the module, or
    /// `cfgd status` and `cfgd module list` both call a fully converged module
    /// "not applied" forever.
    #[test]
    fn a_converged_module_apply_still_records_the_module_as_applied() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let config_dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let config_path = config_dir.path().join("cfgd.yaml");
        std::fs::write(&config_path, CONFIG_YAML).unwrap();
        let profiles_dir = config_dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        // `fakemgr` reports itself present and reports `ripgrep` installed —
        // `echo` runs on every host cfgd targets, so the fixture describes the
        // same machine everywhere and no real package manager is reached.
        std::fs::write(
            profiles_dir.join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  modules:\n    - test-mod\n  packages:\n    custom:\n      - name: fakemgr\n        check: echo ok\n        listInstalled: echo ripgrep\n        install: echo install\n        uninstall: echo uninstall\n",
        )
        .unwrap();
        let mod_dir = config_dir.path().join("modules").join("test-mod");
        std::fs::create_dir_all(&mod_dir).unwrap();
        std::fs::write(
            mod_dir.join("module.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-mod\nspec:\n  packages:\n    - name: ripgrep\n      prefer:\n        - fakemgr\n",
        )
        .unwrap();

        let cli = test_cli_for(config_path, state_dir.path());
        let (apply_printer, apply_buf) = test_printers();
        let args = crate::cli::ApplyArgs {
            on_conflict: crate::cli::OnConflict::Ask,
            from: None,
            dry_run: false,
            phase: None,
            yes: true,
            skip: vec![],
            only: vec![],
            module: vec![],
            with_profile: false,
            skip_scripts: false,
            context: "apply".to_string(),
            shell: None,
        };
        crate::cli::apply::cmd_apply(&cli, &apply_printer, &args).unwrap();
        drop(apply_printer);
        let applied = cfgd_core::test_helpers::captured_text(&apply_buf);

        let store = open_state_store(Some(state_dir.path()), cfgd_core::Scope::User).unwrap();
        let record = store
            .module_state_by_name("test-mod")
            .unwrap()
            .unwrap_or_else(|| {
                panic!("a converged apply must still record module_state, apply said:\n{applied}")
            });
        assert_eq!(record.status, "installed");
        assert!(
            !record.packages_hash.is_empty(),
            "the recorded packages_hash must describe the declared set, got: {record:?}"
        );

        let (printer, buf) = test_printers();
        cmd_status_module(&RunContext::new(&cli, &printer), "test-mod", false, false).unwrap();
        drop(printer);
        let output = cfgd_core::test_helpers::captured_text(&buf);
        assert!(
            output.contains("installed") && !output.contains("not applied"),
            "a converged module must not report itself unapplied, got: {output}"
        );
    }

    /// The inverse of the converged-apply case above: a `--skip` that empties
    /// an otherwise non-empty plan must
    /// not record the module as installed. `fakemgr` here reports the package
    /// absent, so the plan holds a real install action before filtering; the
    /// skip token removes it entirely, leaving a machine that is NOT
    /// converged with nothing left to apply. Recording `module_state` from
    /// that emptied plan would claim a package the machine never received.
    #[test]
    fn a_filter_emptied_plan_does_not_record_the_module_as_applied() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let config_dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let config_path = config_dir.path().join("cfgd.yaml");
        std::fs::write(&config_path, CONFIG_YAML).unwrap();
        let profiles_dir = config_dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        // `fakemgr` reports the package absent, so the reconciler plans a real
        // install before any filter runs.
        std::fs::write(
            profiles_dir.join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  modules:\n    - test-mod\n  packages:\n    custom:\n      - name: fakemgr\n        check: echo ok\n        listInstalled: echo none\n        install: echo install\n        uninstall: echo uninstall\n",
        )
        .unwrap();
        let mod_dir = config_dir.path().join("modules").join("test-mod");
        std::fs::create_dir_all(&mod_dir).unwrap();
        std::fs::write(
            mod_dir.join("module.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-mod\nspec:\n  packages:\n    - name: ripgrep\n      prefer:\n        - fakemgr\n",
        )
        .unwrap();

        let cli = test_cli_for(config_path, state_dir.path());
        let (apply_printer, apply_buf) = test_printers();
        let args = crate::cli::ApplyArgs {
            on_conflict: crate::cli::OnConflict::Ask,
            from: None,
            dry_run: false,
            phase: None,
            yes: true,
            skip: vec!["module:test-mod".to_string()],
            only: vec![],
            module: vec![],
            with_profile: false,
            skip_scripts: false,
            context: "apply".to_string(),
            shell: None,
        };
        crate::cli::apply::cmd_apply(&cli, &apply_printer, &args).unwrap();
        drop(apply_printer);
        let applied = cfgd_core::test_helpers::captured_text(&apply_buf);

        let store = open_state_store(Some(state_dir.path()), cfgd_core::Scope::User).unwrap();
        assert!(
            store.module_state_by_name("test-mod").unwrap().is_none(),
            "a --skip that empties the plan must not mint a converged \
             module_state row for a module the machine never applied, \
             apply said:\n{applied}"
        );
    }

    #[test]
    fn cmd_status_module_without_state_record_prints_not_applied() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let (_cfg_dir, state_dir, config_path) = setup_env_with_module();

        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, buf) = test_printers();

        cmd_status_module(&RunContext::new(&cli, &printer), "test-mod", false, false).unwrap();
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

        cmd_status_module(&RunContext::new(&cli, &printer), "test-mod", false, false).unwrap();
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
            output.contains("/nonexistent/missing.conf") && output.contains("— missing"),
            "missing file should be flagged, got: {output}"
        );
        // No scan ran, so the present file's CONTENT is unchecked and the row
        // must say that rather than claim health `Path::exists` cannot back.
        let present_row = output
            .lines()
            .find(|l| l.contains(real_file.to_str().unwrap()))
            .unwrap_or_else(|| panic!("no row for the present file: {output}"));
        assert!(
            present_row.contains(NOT_SCANNED) && !present_row.contains('✓'),
            "an unscanned present file must not read converged: {present_row}"
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

        cmd_status_module(&RunContext::new(&cli, &printer), "test-mod", false, false).unwrap();
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

    /// A module whose one declared file already matches its deployed copy, so
    /// a live scan of it finds nothing and the only thing left to observe is
    /// what the payload SAYS about having scanned.
    ///
    /// Held as a struct rather than returned loose because every field is a
    /// live guard: dropping a `TempDir` deletes the tree the run is reading,
    /// and dropping the home guard hands the run the real `$HOME`.
    struct ConvergedModuleEnv {
        config_path: std::path::PathBuf,
        state_dir: tempfile::TempDir,
        target: std::path::PathBuf,
        _config_dir: tempfile::TempDir,
        _target_dir: tempfile::TempDir,
        _home: cfgd_core::TestHomeGuard,
    }

    fn converged_module_env() -> ConvergedModuleEnv {
        module_env_with("same content\n", "[]")
    }

    /// `converged_module_env` with the two knobs the state-rendering tests
    /// turn: what the deployed target actually holds (content identical to the
    /// module's source converges, anything else is content drift), and the
    /// module's declared `packages:` block.
    fn module_env_with(target_content: &str, packages_yaml: &str) -> ConvergedModuleEnv {
        let tmp_home = tempfile::tempdir().unwrap();
        let home = cfgd_core::with_test_home_guard(tmp_home.path());
        let config_dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();
        let target = target_dir.path().join("converged.conf");
        std::fs::write(&target, target_content).unwrap();

        let config_path = config_dir.path().join("cfgd.yaml");
        std::fs::write(&config_path, CONFIG_YAML).unwrap();
        let profiles_dir = config_dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(profiles_dir.join("default.yaml"), PROFILE_WITH_MODULE_YAML).unwrap();
        let mod_dir = config_dir.path().join("modules").join("test-mod");
        std::fs::create_dir_all(&mod_dir).unwrap();
        std::fs::write(mod_dir.join("conf"), "same content\n").unwrap();
        let module_yaml = format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-mod\nspec:\n  packages: {}\n  files:\n    - source: conf\n      target: {}\n",
            packages_yaml,
            cfgd_core::to_posix_string(&target)
        );
        std::fs::write(mod_dir.join("module.yaml"), module_yaml).unwrap();

        ConvergedModuleEnv {
            config_path,
            state_dir,
            target,
            _config_dir: config_dir,
            _target_dir: target_dir,
            _home: home,
        }
    }

    /// Record `target` as a file this module deployed, so the Deployed Files
    /// section has a row to state a verdict about.
    fn record_deployed(env: &ConvergedModuleEnv) {
        let store = open_state_store(Some(env.state_dir.path()), cfgd_core::Scope::User).unwrap();
        let apply_id = store
            .record_apply("default", "h", ApplyStatus::Success, None)
            .unwrap();
        store
            .upsert_module_file(
                "test-mod",
                &cfgd_core::to_posix_fs_key(&env.target),
                "hash-deployed",
                "copy",
                apply_id,
            )
            .unwrap();
    }

    /// Everything the report says under its `Deployed Files` heading.
    fn deployed_files_section(output: &str) -> &str {
        output
            .split_once("Deployed Files")
            .unwrap_or_else(|| panic!("no Deployed Files section: {output}"))
            .1
    }

    /// P2: a file whose content drifted is reported drifted by the Drift
    /// section AND by its Deployed Files row. It is present on disk, so the
    /// bare `Path::exists` check this row used to be rendered it converged
    /// three lines under its own `want:`/`have:`.
    #[test]
    fn cmd_status_module_drifted_file_is_never_ok_under_deployed_files() {
        let env = module_env_with("tampered\n", "[]");
        record_deployed(&env);

        let cli = test_cli_for(env.config_path.clone(), env.state_dir.path());
        let (printer, buf) = test_printers();
        cmd_status_module(&RunContext::new(&cli, &printer), "test-mod", false, true).unwrap();
        drop(printer);

        let out = cfgd_core::test_helpers::captured_text(&buf);
        assert!(
            out.contains("want:"),
            "the scan must report the tampered file as drift: {out}"
        );
        let deployed = deployed_files_section(&out);
        let path = cfgd_core::to_posix_string(&env.target);
        let row = deployed
            .lines()
            .find(|l| l.contains(&path))
            .unwrap_or_else(|| panic!("no deployed row for {path}: {out}"));
        assert!(
            row.contains("drifted"),
            "the deployed row must carry the same verdict the Drift section gave: {row}"
        );
        assert!(
            !row.contains('✓'),
            "a drifted file must not render converged: {row}"
        );
    }

    /// The same module with nothing tampered: the row reads converged, so the
    /// drift marking above is a verdict rather than a constant.
    #[test]
    fn cmd_status_module_converged_file_reads_deployed_after_a_scan() {
        let env = converged_module_env();
        record_deployed(&env);

        let cli = test_cli_for(env.config_path.clone(), env.state_dir.path());
        let (printer, buf) = test_printers();
        cmd_status_module(&RunContext::new(&cli, &printer), "test-mod", false, true).unwrap();
        drop(printer);

        let out = cfgd_core::test_helpers::captured_text(&buf);
        let deployed = deployed_files_section(&out);
        let path = cfgd_core::to_posix_string(&env.target);
        let row = deployed
            .lines()
            .find(|l| l.contains(&path))
            .unwrap_or_else(|| panic!("no deployed row for {path}: {out}"));
        assert!(
            row.contains("deployed") && !row.contains("drifted"),
            "a scanned, converged file must read deployed: {row}"
        );
    }

    /// P1: the packages phase has a STATE presentation, not just a declared
    /// count. A `script` package is the deterministic arm — no manager can be
    /// asked about it on any host — so the row names the manager that would
    /// have answered and says plainly that nothing did.
    #[test]
    fn cmd_status_module_scan_renders_package_state_per_declared_package() {
        let env = module_env_with(
            "same content\n",
            "\n    - name: rustup\n      prefer:\n        - script\n      script: \"true\"",
        );
        let cli = test_cli_for(env.config_path.clone(), env.state_dir.path());
        let (printer, buf) = test_printers();
        cmd_status_module(&RunContext::new(&cli, &printer), "test-mod", false, true).unwrap();
        drop(printer);

        let out = cfgd_core::test_helpers::captured_text(&buf);
        assert!(
            out.contains("\nPackages\n"),
            "the packages phase must have a section of its own: {out}"
        );
        let row = out
            .lines()
            .find(|l| l.contains("rustup"))
            .unwrap_or_else(|| panic!("no package row: {out}"));
        assert!(
            row.contains(NOT_SCANNED) && row.contains("(script)"),
            "the row must name the manager and the verdict it gave: {row}"
        );
    }

    /// Without `--scan` the section still stands — a declared package with no
    /// state is still a package the apply installed — and every row says
    /// nothing asked, rather than borrowing the ✓ a scan would have earned.
    #[test]
    fn cmd_status_module_without_scan_lists_declared_packages_as_unscanned() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let (_cfg_dir, state_dir, config_path) = setup_env_with_module();

        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, buf) = test_printers();
        cmd_status_module(&RunContext::new(&cli, &printer), "test-mod", false, false).unwrap();
        drop(printer);

        let out = cfgd_core::test_helpers::captured_text(&buf);
        let row = out
            .lines()
            .find(|l| l.contains("ripgrep"))
            .unwrap_or_else(|| panic!("the declared package must appear: {out}"));
        assert!(
            row.contains(NOT_SCANNED) && !row.contains('✓'),
            "an unscanned package must not read installed: {row}"
        );
    }

    /// `--scan` without `-e` scans, and the payload must say so.
    ///
    /// The two flags are separate now, and every other module test passes them
    /// together — which is exactly the pairing under which a payload reporting
    /// the WRONG one of the two still reads correct. A consumer differencing
    /// an empty `drift` array has only this flag to tell "checked, and the
    /// machine is clean" from "never checked".
    #[test]
    fn cmd_status_module_scan_without_exit_code_reports_the_scan_it_ran() {
        let env = converged_module_env();
        let mut cli = test_cli_for(env.config_path.clone(), env.state_dir.path());
        cli.output = OutputFormatArg(cfgd_core::output::OutputFormat::Json);
        let (printer, buf) = test_printers_json();

        cmd_status_module(&RunContext::new(&cli, &printer), "test-mod", false, true).unwrap();
        drop(printer);

        let captured = cfgd_core::test_helpers::captured_text(&buf);
        let parsed: serde_json::Value = serde_json::from_str(captured.trim())
            .unwrap_or_else(|e| panic!("invalid JSON: {e}, got: {captured}"));
        assert_eq!(
            parsed["driftCheckedLive"], true,
            "`--scan` ran the live scan, so the payload must not report otherwise: {parsed}"
        );
        assert_eq!(
            parsed["drift"],
            serde_json::json!([]),
            "a converged module's live scan must find nothing, got: {parsed}"
        );
    }

    // The drift-catching, exit(5) branch is proven by the real subprocess in
    // `tests/cli_integration.rs::status_module_exit_code_catches_module_file_drift`
    // — `process::exit` cannot be exercised in-process. This test proves the
    // complementary path: a converged module's live scan finds nothing, so
    // `--exit-code` must return Ok rather than calling `process::exit`.
    #[test]
    fn cmd_status_module_exit_code_true_no_drift_returns_ok() {
        let env = converged_module_env();
        let mut cli = test_cli_for(env.config_path.clone(), env.state_dir.path());
        cli.output = OutputFormatArg(cfgd_core::output::OutputFormat::Json);
        let (printer, buf) = test_printers_json();

        let res = cmd_status_module(&RunContext::new(&cli, &printer), "test-mod", true, true);
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
