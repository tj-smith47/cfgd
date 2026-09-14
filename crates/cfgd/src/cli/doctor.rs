use super::*;
use crate::cli::output_types::DoctorConfigState;
use cfgd_core::PathDisplayExt;
use cfgd_core::output::{Doc, Printer, Role, doc::SectionBuilder};
use cfgd_core::providers::PackageManagerExt;

pub(super) fn cmd_doctor(cli: &Cli, printer: &Printer, fix: bool) -> anyhow::Result<()> {
    // A failed verdict must fail the process so `cfgd doctor && cfgd apply`
    // stops instead of sailing into a guaranteed-broken apply. The Doc is
    // already emitted, so exit directly (mirroring cmd_profile_migrate)
    // rather than return an error the central sink would re-render.
    if !run_doctor(cli, printer, fix)? {
        cfgd_core::exit::ExitCode::Error.exit();
    }
    Ok(())
}

/// The tools a check reports on, with the `CFGD_*_BIN` seam each is reached
/// through (`""` for a tool with none).
///
/// `--fix` installs exactly what the rows above report, so a tool added to the
/// Tools or Secrets section joins this list with it. The optional secret
/// providers are deliberately absent: their rows say "optional", and a reader
/// asking cfgd to repair its prerequisites did not ask for four vendor CLIs.
const FIXABLE_TOOLS: &[(&str, &str)] = &[("git", ""), ("sops", "CFGD_SOPS_BIN")];

/// Install every tool of [`FIXABLE_TOOLS`] this host is missing, before the
/// probes run.
///
/// Ahead of the probes rather than after them, so the report a reader ends up
/// looking at states the machine as `--fix` left it, not as it was found.
fn fix_missing_tools(printer: &Printer) {
    let missing: Vec<&(&str, &str)> = FIXABLE_TOOLS
        .iter()
        // provision-route: cfgd doctor --fix, which is the loop below
        .filter(|(tool, seam)| cfgd_core::require_tool_with_seam(seam, tool, None).is_err())
        .collect();
    if missing.is_empty() {
        return;
    }
    let section = printer.section("Install missing tools");
    // The install commits live rows of its own through the printer, which is a
    // top-level emit while this section is open unless the depth is inherited.
    let _inherit = printer.depth_inheritance();
    let registry = crate::cli::build_registry();
    for (tool, seam) in missing {
        match crate::cli::helpers::provision_tool(printer, &registry, tool, seam) {
            // name-row-ok: the row names the executable, not an outcome
            Ok(()) => {
                section.status(Role::Ok, *tool).qualifier("installed");
            }
            // name-row-ok: the row names the executable, not an outcome
            Err(reason) => {
                // no-next-step: the reason lists the managers that would have
                // installed it, which is the only move left to the reader
                section.status(Role::Fail, *tool).detail(reason);
            }
        }
    }
}

/// Runs every doctor probe, emits the report Doc, and returns whether the
/// verdict passed. Kept separate from the process-exit wrapper so it stays
/// unit-testable.
pub(crate) fn run_doctor(cli: &Cli, printer: &Printer, fix: bool) -> anyhow::Result<bool> {
    if fix {
        fix_missing_tools(printer);
    }
    // One spinner across every probe, renamed per group: doctor shells out to
    // git, sops and each package manager before it prints anything at all.
    let (output, extras) = printer.narrate("Probing: config", |sp| {
        collect_doctor_output(cli, printer, sp)
    })?;
    let passed = all_passed(&output);
    printer.emit(build_doctor_doc(&output, &extras));
    Ok(passed)
}

/// Display-only doctor results that are not part of the stable JSON payload.
///
/// The `DoctorOutput` schema is consumer-facing and frozen; this struct carries
/// the human-section sources (state-store health, profiles dir, config sources)
/// so the human Doc keeps parity with the prior output without altering the
/// `-o json` shape.
#[derive(Default)]
pub struct DoctorExtras {
    pub state_store: Option<DoctorStateStore>,
    pub profiles_dir: Option<DoctorProfilesDir>,
    pub config_sources: Vec<DoctorConfigSource>,
    /// The env var currently suppressing the automatic update check
    /// (`CFGD_NO_UPDATE_CHECK` / `NO_UPDATE_NOTIFIER` / `DO_NOT_TRACK`), or
    /// `None` when no opt-out is active.
    pub update_optout: Option<&'static str>,
}

pub struct DoctorStateStore {
    pub accessible: bool,
    pub message: Option<String>,
}

pub struct DoctorProfilesDir {
    pub path: String,
    pub exists: bool,
    pub profile_count: usize,
    /// Set when the directory exists but could not be enumerated
    /// (e.g. permission denied); the count is meaningless then.
    pub error: Option<String>,
}

pub struct DoctorConfigSource {
    pub name: String,
    pub cached_path: Option<String>,
}

/// Gather every doctor check into the stable JSON payload + display-only extras.
/// The lib call to `modules::load_all_modules` takes a `Printer`.
fn collect_doctor_output(
    cli: &Cli,
    printer: &Printer,
    sp: &mut cfgd_core::output::Spinner<'_>,
) -> anyhow::Result<(DoctorOutput, DoctorExtras)> {
    let ctx = RunContext::new(cli, printer);
    let (config_check, loaded_cfg) = if cli.config.exists() {
        match config::load_config(&cli.config) {
            Ok(mut cfg) => {
                drain_config_deprecations(printer, &mut cfg);
                (
                    DoctorConfigCheck {
                        valid: true,
                        path: cfgd_core::to_posix_string(&cli.config),
                        name: Some(cfg.metadata.name.clone()),
                        profile: cfg.spec.profile.clone(),
                        error: None,
                        state: DoctorConfigState::Valid,
                    },
                    Some(cfg),
                )
            }
            Err(e) => (
                DoctorConfigCheck {
                    valid: false,
                    path: cfgd_core::to_posix_string(&cli.config),
                    name: None,
                    profile: None,
                    error: Some(format!("{}", e)),
                    state: DoctorConfigState::Invalid,
                },
                None,
            ),
        }
    } else {
        // Missing at the derived default path = fresh machine (Warn, verdict
        // passes); missing at a user-supplied path = the user's typo (Fail,
        // verdict fails). The JSON `error` string stays "not found" for both
        // so the serialized shape is unchanged; the typed state carries the
        // distinction.
        let state = if cli.config_explicit {
            DoctorConfigState::MissingAtExplicit
        } else {
            DoctorConfigState::MissingAtDefault
        };
        (
            DoctorConfigCheck {
                valid: false,
                path: cfgd_core::to_posix_string(&cli.config),
                name: None,
                profile: None,
                error: Some(cfgd_core::Absence::NotFound.to_string()),
                state,
            },
            None,
        )
    };

    sp.set_message("Probing: tools");
    let git_available = cfgd_core::command_available("git");

    let config_dir = config_dir(cli);
    let age_key_override = loaded_cfg
        .as_ref()
        .and_then(|c| c.spec.secrets.as_ref())
        .and_then(|s| s.sops.as_ref())
        .and_then(|s| s.age_key.as_ref());

    let health = secrets::check_secrets_health(&config_dir, age_key_override.map(|p| p.as_path()));

    // Resolved ONCE and read by both the package report below and the module
    // list further down: `doctor` asked the same question twice, and a profile
    // resolution walks the inheritance chain off disk each time.
    let doctor_profile = loaded_cfg.as_ref().and_then(|cfg| {
        let profiles_dir = profiles_dir(cli);
        let profile_name = cli.profile.as_deref().or(cfg.spec.profile.as_deref())?;
        config::resolve_profile(profile_name, &profiles_dir).ok()
    });

    let resolved_packages = doctor_profile.as_ref().map(|resolved| {
        let mut packages = resolved.merged.packages.clone();
        if let Err(e) = ctx.resolve_manifest_packages(&mut packages) {
            // Manifest resolution failed (missing referenced file, unreadable
            // dir, parse error). Surface so the user knows the package report
            // below is computed from a partial set.
            printer
                .status(Role::Warn, "Manifest resolution failed")
                .qualifier(cfgd_core::output::collapse_to_subject_line(&e))
                .detail("package report may be incomplete");
        }
        packages
    });

    let registry = if let Some(ref pkgs) = resolved_packages {
        build_registry_with_profile(pkgs)
    } else {
        build_registry()
    };
    sp.set_message("Probing: package managers");
    let all_managers = registry.package_managers();

    let declared_managers: Vec<String> = if let Some(ref pkgs) = resolved_packages {
        let mut declared = Vec::new();
        if let Some(ref brew) = pkgs.brew
            && (!brew.formulae.is_empty() || !brew.taps.is_empty() || !brew.casks.is_empty())
        {
            declared.push("brew".to_string());
        }
        if let Some(ref apt) = pkgs.apt
            && !apt.packages.is_empty()
        {
            declared.push("apt".to_string());
        }
        if let Some(ref cargo) = pkgs.cargo
            && !cargo.packages.is_empty()
        {
            declared.push("cargo".to_string());
        }
        if let Some(ref npm) = pkgs.npm
            && !npm.global.is_empty()
        {
            declared.push("npm".to_string());
        }
        for (name, _) in pkgs.non_empty_simple_lists() {
            declared.push(name.to_string());
        }
        if let Some(ref snap) = pkgs.snap
            && !snap.packages.is_empty()
        {
            declared.push("snap".to_string());
        }
        if let Some(ref flatpak) = pkgs.flatpak
            && !flatpak.packages.is_empty()
        {
            declared.push("flatpak".to_string());
        }
        for custom in &pkgs.custom {
            if !custom.packages.is_empty() {
                declared.push(custom.name.clone());
            }
            if custom.name.contains('.') {
                printer
                    .status(
                        Role::Warn,
                        format!(
                            "Custom manager '{}' contains '.' in its name: source-delivered packages under it cannot carry decisions (the decision path grammar splits on '.') and are withheld from every run",
                            custom.name
                        ),
                    )
                    .detail("rename it to be asked about them");
            }
        }
        declared
    } else {
        Vec::new()
    };

    let module_list: Vec<String> = doctor_profile
        .as_ref()
        .map(|r| r.merged.modules.clone())
        .unwrap_or_default();

    let cache_base = module_cache_dir(cli).unwrap_or_default();
    sp.set_message("Probing: modules");
    let all_modules =
        modules::load_all_modules(&config_dir, &cache_base, &[], printer).unwrap_or_default();

    // Per-module prerequisite detail: resolve each declared package to the
    // manager that would deliver it, so the row below can state whether that
    // manager is on this host. `doctor` checks prerequisites, so it never asks
    // whether the package itself is installed.
    //
    // Deliberately the config-FREE registry, and the one place in the run that
    // wants a second one: the package report above builds a config-aware
    // registry from the resolved profile, which registers the profile's
    // `packages.custom` managers. A MODULE cannot reach those — it resolves
    // against the managers it declares — so resolving the module report through
    // the profile's registry would report a module package as resolvable by a
    // manager the module cannot use.
    let modules_registry = ctx.base_registry();
    let mgr_map = modules_registry.manager_map();
    let platform = Platform::current();
    let doctor_cx = ctx.package_context().ok();

    // Manager name to the modules routing to it, filled by the one resolution
    // below and read again by the Package Managers section, which would
    // otherwise resolve every module package a second time to answer the same
    // question.
    let mut module_routes: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
        std::collections::BTreeMap::new();

    let module_checks: Vec<DoctorModuleCheck> = module_list
        .iter()
        .map(|mod_name| {
            let Some(module) = all_modules.get(mod_name) else {
                return DoctorModuleCheck {
                    name: mod_name.clone(),
                    valid: false,
                    error: Some(format!("module {}", cfgd_core::Absence::NotFound)),
                    managers: Vec::new(),
                    unresolved: Vec::new(),
                };
            };
            // First-seen order, which is the module's own package order: the
            // row reads as the author listed them.
            let mut order: Vec<String> = Vec::new();
            let mut counts: std::collections::HashMap<String, usize> =
                std::collections::HashMap::new();
            let mut unresolved: Vec<String> = Vec::new();
            for entry in &module.spec.packages {
                match modules::resolve_package(
                    entry,
                    mod_name,
                    platform,
                    &mgr_map,
                    doctor_cx.as_ref(),
                ) {
                    Ok(Some(resolved)) => {
                        let count = counts.entry(resolved.manager.clone()).or_insert(0);
                        if *count == 0 {
                            order.push(resolved.manager.clone());
                        }
                        *count += 1;
                        module_routes
                            .entry(resolved.manager)
                            .or_default()
                            .insert(mod_name.clone());
                    }
                    // Gated off this platform: the package is not declared
                    // here, so it routes nowhere and states nothing.
                    Ok(None) => {}
                    Err(e) => unresolved.push(e.to_string()),
                }
            }
            let managers = order
                .into_iter()
                .map(|name| DoctorModuleManagerRoute {
                    available: mgr_map.get(&name).is_some_and(|m| m.is_available()),
                    package_count: counts.get(&name).copied().unwrap_or(0),
                    name,
                })
                .collect();
            DoctorModuleCheck {
                name: mod_name.clone(),
                valid: true,
                error: None,
                managers,
                unresolved,
            }
        })
        .collect();

    // Deduplicate brew-tap / brew-cask under the parent brew manager so the
    // human + structured output shows brew once.
    let mut manager_checks: Vec<DoctorManagerCheck> = Vec::new();
    {
        let mut seen = std::collections::HashSet::new();
        for mgr in all_managers.iter() {
            let name = mgr.name();
            if name == "brew-tap" || name == "brew-cask" {
                continue;
            }
            if !seen.insert(name.to_string()) {
                continue;
            }
            // The one site that needs the plan itself rather than the question
            // `PackageManagerExt::can_bootstrap` answers — it reports the method
            // beside the flag, so asking the manager twice would re-derive a plan
            // already in hand.
            let plan = mgr.bootstrap_plan();
            let can_bootstrap = plan.is_some();
            let bootstrap_method = plan.map(|p| p.method);
            manager_checks.push(DoctorManagerCheck {
                name: name.to_string(),
                available: mgr.is_available(),
                declared: declared_managers.iter().any(|d| d == name),
                can_bootstrap,
                bootstrap_method,
                used_by_modules: module_routes.get(name).map_or(0, |m| m.len()),
            });
        }
    }

    let configurator_checks: Vec<DoctorConfiguratorCheck> = registry
        .available_system_configurators()
        .iter()
        .map(|c| DoctorConfiguratorCheck {
            name: c.name().to_string(),
            available: true,
        })
        .collect();

    // Probe the store THIS run's `--state-dir`/`--scope` would open, not the
    // per-user default — a `--scope system` doctor reporting the user store
    // accessible would be diagnosing a store the run never uses. Asked through
    // the run's context, which opens that exact store and does NOT memoize a
    // failure, so a refused open is still re-attempted and still reported here
    // rather than being answered from a cached error.
    sp.set_message("Probing: state store");
    let state_store = match ctx.state() {
        Ok(_) => DoctorStateStore {
            accessible: true,
            message: None,
        },
        Err(e) => DoctorStateStore {
            accessible: false,
            message: Some(e.to_string()),
        },
    };

    sp.set_message("Probing: profiles");
    let profiles_dir_path = profiles_dir(cli);
    // One ambiguity-tolerant walk feeds both the System count and the
    // per-profile layout checks, so the two can never disagree on what counts
    // as a profile (canonical bundles included, payload dirs excluded).
    let profiles_scan = cfgd_core::config::scan_profiles_tolerant(&profiles_dir_path);
    let profiles_dir_extra = DoctorProfilesDir {
        // absolute-path-ok: the payload field; the rows rendering it fold their own copy
        path: profiles_dir_path.display_posix(),
        exists: profiles_dir_path.exists(),
        profile_count: profiles_scan.as_ref().map(Vec::len).unwrap_or(0),
        error: profiles_scan.as_ref().err().map(|e| e.to_string()),
    };

    let config_sources: Vec<DoctorConfigSource> = if cli.config.exists()
        && let Ok(cfg) = config::load_config(&cli.config)
        && !cfg.spec.sources.is_empty()
    {
        let cache_dir = source_cache_dir(cli).ok();
        cfg.spec
            .sources
            .iter()
            .map(|source| {
                let cached_path = cache_dir.as_ref().and_then(|cd| {
                    let p = cd.join(&source.name);
                    if p.exists() {
                        // absolute-path-ok: the payload field; the row rendering it folds its own copy
                        Some(p.display_posix())
                    } else {
                        None
                    }
                });
                DoctorConfigSource {
                    name: source.name.clone(),
                    cached_path,
                }
            })
            .collect()
    } else {
        Vec::new()
    };

    let profile_layouts: Vec<DoctorProfileLayoutCheck> = match &profiles_scan {
        Ok(entries) => entries
            .iter()
            .map(|entry| match entry {
                cfgd_core::config::ProfileScanEntry::Found(found) => DoctorProfileLayoutCheck {
                    name: found.name.clone(),
                    legacy: found.form == cfgd_core::config::ProfileForm::LegacyFlat,
                    path: Some(cfgd_core::to_posix_string(&found.path)),
                    error: None,
                },
                cfgd_core::config::ProfileScanEntry::Ambiguous { name, error, .. } => {
                    DoctorProfileLayoutCheck {
                        name: name.clone(),
                        legacy: true,
                        path: None,
                        error: Some(error.to_string()),
                    }
                }
            })
            .collect(),
        // An unreadable profiles dir is a hard failure, not "no profiles" —
        // surface it as a failing check so it flips the doctor verdict.
        Err(e) => vec![DoctorProfileLayoutCheck {
            name: cfgd_core::to_posix_string(&profiles_dir_path),
            legacy: false,
            path: None,
            error: Some(e.to_string()),
        }],
    };

    let output = DoctorOutput {
        config: config_check,
        git: git_available,
        secrets: DoctorSecretsCheck {
            sops_available: health.sops_available,
            sops_version: health.sops_version.clone(),
            age_key_exists: health.age_key_exists,
            age_key_path: health.age_key_path.as_ref().map(cfgd_core::to_posix_string),
            sops_config_exists: health.sops_config_exists,
            sops_config_path: health
                .sops_config_path
                .as_ref()
                .map(cfgd_core::to_posix_string),
            providers: health
                .providers
                .iter()
                .map(|(n, a)| DoctorProviderCheck {
                    name: n.clone(),
                    available: *a,
                })
                .collect(),
        },
        package_managers: manager_checks,
        modules: module_checks,
        system_configurators: configurator_checks,
        profiles: profile_layouts,
    };

    let extras = DoctorExtras {
        state_store: Some(state_store),
        profiles_dir: Some(profiles_dir_extra),
        config_sources,
        update_optout: cfgd_core::upgrade::update_optout_var(),
    };

    Ok((output, extras))
}

/// Build the doctor `Doc` from a collected payload + display-only extras. Used
/// by the live command and by snapshot tests under
/// `tests/output_snapshots/doctor/`.
// no-next-step: `doctor` is the diagnosis; every failing row below carries its own fix in its detail
pub fn build_doctor_doc(output: &DoctorOutput, extras: &DoctorExtras) -> Doc {
    let mut doc = Doc::new().heading("Doctor");

    doc = doc.section("Config", |s| build_config_section(s, &output.config));
    doc = doc.section("Tools", |s| build_tools_section(s, output.git));
    doc = doc.section("Secrets", |s| build_secrets_section(s, &output.secrets));
    doc = doc.section_if_nonempty(
        "Package Managers",
        &output.package_managers,
        build_managers_section,
    );
    doc = doc.section_if_nonempty("Modules", &output.modules, build_modules_section);
    doc = doc.section_if_nonempty("Profiles", &output.profiles, build_profiles_section);
    // `Installation`, never `System`: `diff` and `status` reserve a `System`
    // section for system-configurator drift, and these rows check none of it —
    // they are cfgd's own state store, profiles directory and update behaviour
    // on this machine. `Environment` is spoken for too; it reads as the `spec.env`
    // surface, which doctor equally does not check.
    doc = doc.section("Installation", |s| build_installation_section(s, extras));
    doc = doc.section_if_nonempty(
        super::source::list::SOURCES_SECTION,
        &extras.config_sources,
        build_sources_section,
    );

    if all_passed(output) {
        doc = doc.status(Role::Ok, "Passed every check");
    } else {
        doc = doc.status_with(Role::Fail, "Some checks failed", |f| f.detail("see above"));
    }

    doc.with_data(output)
}

// no-next-step: the row's detail names the file to fix
fn build_config_section(s: SectionBuilder, cfg: &DoctorConfigCheck) -> SectionBuilder {
    match cfg.state {
        DoctorConfigState::Valid => {
            // name-row-ok: an inventory row naming what was checked
            let mut s = s.status_with(Role::Ok, "Config file", |f| {
                f.qualifier(format!(
                    "{} (valid)",
                    cfgd_core::fold_home_in_text(&cfg.path)
                ))
            });
            let mut pairs: Vec<(String, String)> = Vec::new();
            if let Some(name) = cfg.name.as_deref() {
                pairs.push(("Name".into(), name.into()));
            }
            pairs.push((
                "Profile".into(),
                cfg.profile.as_deref().unwrap_or("(none)").into(),
            ));
            // facts-block-ok: the block closes this arm's section; the rows
            // below are the match's other arms, not rows after it
            s = s.kv_block(pairs);
            s
        }
        DoctorConfigState::MissingAtDefault => s.status_with(Role::Warn, "Config file", |sf| {
            sf.qualifier(cfgd_core::fold_home_in_text(&cfg.path))
                .detail(format!(
                    "{}; run `cfgd init` to create one",
                    cfgd_core::Absence::NotFound
                ))
        }),
        DoctorConfigState::MissingAtExplicit => s.status_with(Role::Fail, "Config file", |sf| {
            sf.qualifier(cfgd_core::fold_home_in_text(&cfg.path))
                .detail(format!(
                    "{}; the given --config/--config-dir/CFGD_CONFIG path does not exist",
                    cfgd_core::Absence::NotFound
                ))
        }),
        DoctorConfigState::Invalid => s.status_with(Role::Fail, "Config file", |f| {
            f.qualifier(cfgd_core::fold_home_in_text(&cfg.path))
                .detail(cfg.error.as_deref().unwrap_or("invalid").to_string())
        }),
    }
}

// no-next-step: the row's detail names the command that installs the tool
fn build_tools_section(s: SectionBuilder, git_available: bool) -> SectionBuilder {
    if git_available {
        // name-row-ok: the row names the executable, not an outcome
        s.status_with(Role::Ok, "git", |f| f.qualifier("found"))
    } else {
        // name-row-ok: the row names the executable, not an outcome
        s.status_with(Role::Fail, "git", |f| {
            f.qualifier(cfgd_core::Absence::NotFound.as_str())
                .detail(MSG_RUN_DOCTOR_FIX)
        })
    }
}

/// What a row naming a missing tool says to do about it.
///
/// One sentence for every such row, because the answer is the same whichever
/// tool is missing: cfgd installs it through the manager this host already has.
const MSG_RUN_DOCTOR_FIX: &str = "run `cfgd doctor --fix` to install it";

fn build_secrets_section(mut s: SectionBuilder, secrets: &DoctorSecretsCheck) -> SectionBuilder {
    s = if secrets.sops_available {
        let version_str = secrets.sops_version.as_deref().unwrap_or("unknown version");
        // name-row-ok: the row names the executable, not an outcome
        s.status_with(Role::Ok, "sops", |f| {
            f.qualifier(format!("found ({})", version_str))
        })
    } else {
        // name-row-ok: the row names the executable, not an outcome
        s.status_with(Role::Warn, "sops", |f| {
            f.qualifier(cfgd_core::Absence::NotFound.as_str())
                .detail(format!("required for secrets; {MSG_RUN_DOCTOR_FIX}"))
        })
    };

    s = match (secrets.age_key_exists, secrets.age_key_path.as_deref()) {
        // name-row-ok: the row names the key file, not an outcome
        (true, Some(path)) => s.status_with(Role::Ok, "age key", |f| {
            f.qualifier(cfgd_core::fold_home_in_text(path))
        }),
        // name-row-ok: the row names the key file, not an outcome
        (false, Some(path)) => s.status_with(Role::Warn, "age key", |f| {
            f.qualifier(cfgd_core::fold_home_in_text(path))
                .detail(format!(
                    "{}; run `cfgd init` to generate",
                    cfgd_core::Absence::NotFound
                ))
        }),
        _ => s,
    };

    s = match (
        secrets.sops_config_exists,
        secrets.sops_config_path.as_deref(),
    ) {
        (true, Some(path)) => {
            // name-row-ok: an inventory row naming what was checked
            s.status_with(Role::Ok, ".sops.yaml", |f| {
                f.qualifier(cfgd_core::fold_home_in_text(path))
            })
        }
        // name-row-ok: an inventory row naming what was checked
        (true, None) => s.status_with(Role::Ok, ".sops.yaml", |f| f.qualifier("present")),
        (false, _) => s.status_with(Role::Warn, ".sops.yaml", |f| {
            f.qualifier(cfgd_core::Absence::NotFound.as_str())
                .detail("will be generated on `cfgd init`")
        }),
    };

    for provider in &secrets.providers {
        s = if provider.available {
            // name-row-ok: an inventory row naming the provider
            s.status_with(Role::Ok, format!("Provider {}", provider.name), |f| {
                f.qualifier("available")
            })
        } else {
            s.status_with(Role::Info, format!("Provider {}", provider.name), |f| {
                // presence-row-ok: a secret backend is a TOOL this host either
                // has or does not, the same question as `git` above — not
                // whether a declared package the config manages reached the
                // machine.
                f.qualifier(format!("{} (optional)", cfgd_core::Absence::NotInstalled))
            })
        };
    }
    s
}

// no-next-step: the row's detail names what the manager reported
fn build_managers_section(s: SectionBuilder, managers: &[DoctorManagerCheck]) -> SectionBuilder {
    managers.iter().fold(s, |s, m| {
        if m.declared {
            if m.available {
                s.status_with(Role::Ok, m.name.clone(), |sf| {
                    sf.qualifier("available (declared in config)")
                })
            } else if m.can_bootstrap {
                let detail = match m.bootstrap_method.as_deref() {
                    Some(method) => format!("can auto-bootstrap via {}", method),
                    None => "can auto-bootstrap".into(),
                };
                s.status_with(Role::Warn, m.name.clone(), |sf| {
                    sf.qualifier(cfgd_core::Absence::NotFound.as_str())
                        .detail(detail)
                })
            } else {
                s.status_with(Role::Fail, m.name.clone(), |sf| {
                    sf.qualifier(cfgd_core::Absence::NotFound.as_str())
                        .detail("declared in config but not available")
                })
            }
        } else if m.available {
            let note = if m.used_by_modules == 0 {
                "available (not used)".to_string()
            } else {
                format!(
                    "available (used by {})",
                    cfgd_core::pluralize(m.used_by_modules, "module")
                )
            };
            s.status_with(Role::Info, m.name.clone(), |sf| sf.qualifier(note))
        } else {
            s
        }
    })
}

// no-next-step: the row's detail names the manager the module's packages need
fn build_modules_section(s: SectionBuilder, modules: &[DoctorModuleCheck]) -> SectionBuilder {
    modules.iter().fold(s, |s, m| {
        if !m.valid {
            let detail = m.error.clone().unwrap_or_else(|| "invalid".into());
            return s.status_with(Role::Fail, m.name.clone(), |sf| sf.detail(detail));
        }
        if m.managers.is_empty() && m.unresolved.is_empty() {
            return s.status(Role::Ok, m.name.clone());
        }
        let mut shortfalls: Vec<String> = m
            .managers
            .iter()
            .filter(|r| !r.available)
            .map(|r| {
                format!(
                    "{} missing ({} route to it)",
                    r.name,
                    cfgd_core::pluralize(r.package_count, "package")
                )
            })
            .collect();
        shortfalls.extend(m.unresolved.iter().cloned());
        if shortfalls.is_empty() {
            let names: Vec<&str> = m.managers.iter().map(|r| r.name.as_str()).collect();
            let detail = format!("{} available", names.join(", "));
            return s.status_with(Role::Ok, m.name.clone(), |sf| sf.detail(detail));
        }
        s.status_with(Role::Fail, m.name.clone(), |sf| {
            sf.detail(shortfalls.join(", "))
        })
    })
}

// no-next-step: the row's detail names the migrate command for a legacy profile
fn build_profiles_section(
    s: SectionBuilder,
    profiles: &[DoctorProfileLayoutCheck],
) -> SectionBuilder {
    if profiles.iter().all(|p| !p.legacy && p.error.is_none()) {
        // verdict-row-ok: a layout verdict, not an act cfgd performed
        return s.status(Role::Ok, "All profiles use the canonical bundle layout");
    }
    profiles.iter().fold(s, |s, p| {
        if let Some(err) = p.error.as_deref() {
            // Ambiguous / unscannable profiles are hard-broken (every load of
            // them errors), unlike the supported legacy form — Fail, not Warn.
            s.status(Role::Fail, cfgd_core::output::collapse_to_subject_line(err))
        } else if p.legacy {
            // name-row-ok: the row names the profile, not an outcome
            s.status_with(Role::Warn, format!("profile '{}'", p.name), |sf| {
                sf.qualifier("uses the legacy flat layout")
                    .detail(format!("run `cfgd profile migrate {}`", p.name))
            })
        } else {
            s.status(Role::Ok, p.name.clone())
        }
    })
}

// no-next-step: the row's detail names the directory cfgd could not use
fn build_installation_section(mut s: SectionBuilder, extras: &DoctorExtras) -> SectionBuilder {
    if let Some(ss) = extras.state_store.as_ref() {
        s = if ss.accessible {
            // name-row-ok: an inventory row naming what was checked
            s.status(Role::Ok, "State store: accessible")
        } else {
            let detail = ss.message.clone().unwrap_or_else(|| "unavailable".into());
            s.status_with(Role::Warn, "State store: unavailable", |sf| {
                sf.detail(detail)
            })
        };
    }
    if let Some(pd) = extras.profiles_dir.as_ref() {
        s = if let Some(err) = pd.error.as_deref() {
            s.status_with(Role::Fail, "Profiles directory", |sf| {
                sf.qualifier(cfgd_core::fold_home_in_text(&pd.path))
                    .detail(cfgd_core::output::collapse_to_subject_line(err))
            })
        } else if pd.exists {
            // name-row-ok: an inventory row naming what was checked
            s.status_with(Role::Ok, "Profiles directory", |sf| {
                sf.qualifier(format!(
                    "{} ({})",
                    cfgd_core::fold_home_in_text(&pd.path),
                    cfgd_core::pluralize(pd.profile_count, "profile")
                ))
            })
        } else {
            s.status_with(
                Role::Warn,
                format!("Profiles directory {}", cfgd_core::Absence::NotFound),
                |sf| sf.qualifier(cfgd_core::fold_home_in_text(&pd.path)),
            )
        };
    }
    if let Some(var) = extras.update_optout {
        s = s.status_with(Role::Info, "Automatic update check", |sf| {
            sf.qualifier(format!("suppressed by {var}"))
        });
    }
    s
}

fn build_sources_section(s: SectionBuilder, sources: &[DoctorConfigSource]) -> SectionBuilder {
    sources
        .iter()
        .fold(s, |s, source| match source.cached_path.as_deref() {
            Some(path) => s.status_with(Role::Ok, source.name.clone(), |f| {
                f.qualifier(format!("cached at {}", cfgd_core::fold_home_in_text(path)))
            }),
            None => s.status_with(Role::Warn, source.name.clone(), |f| {
                f.qualifier("not cached (run `cfgd source update`)")
            }),
        })
}

fn all_passed(output: &DoctorOutput) -> bool {
    config_ok(&output.config)
        && output.git
        && output
            .package_managers
            .iter()
            .all(|m| !m.declared || m.available || m.can_bootstrap)
        && output.modules.iter().all(|m| {
            m.valid
                && m.unresolved.is_empty()
                && m.managers.iter().all(|r| r.available)
        })
        // Legacy layout is a Warn (supported); only errored profile checks
        // (ambiguous forms, unscannable dir) fail the verdict.
        && output.profiles.iter().all(|p| p.error.is_none())
}

/// A config missing at the DEFAULT path is a fresh-machine state (rendered as
/// a Warn), not a failure. A config missing at an explicitly-given path, or a
/// present-but-unparseable one, fails the verdict. Mirrors the classification
/// in `build_config_section`.
fn config_ok(cfg: &DoctorConfigCheck) -> bool {
    matches!(
        cfg.state,
        DoctorConfigState::Valid | DoctorConfigState::MissingAtDefault
    )
}
