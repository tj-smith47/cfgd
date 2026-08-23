use super::*;
use crate::cli::output_types::DoctorConfigState;
use cfgd_core::output::{Doc, Printer, Role, doc::SectionBuilder};

pub(super) fn cmd_doctor(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    // A failed verdict must fail the process so `cfgd doctor && cfgd apply`
    // stops instead of sailing into a guaranteed-broken apply. The Doc is
    // already emitted, so exit directly (mirroring cmd_profile_migrate)
    // rather than return an error the central sink would re-render.
    if !run_doctor(cli, printer)? {
        cfgd_core::exit::ExitCode::Error.exit();
    }
    Ok(())
}

/// Runs every doctor probe, emits the report Doc, and returns whether the
/// verdict passed. Kept separate from the process-exit wrapper so it stays
/// unit-testable.
pub(crate) fn run_doctor(cli: &Cli, printer: &Printer) -> anyhow::Result<bool> {
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

/// Whether `manager` reports `resolved_name` installed.
///
/// The answer comes from the context's memo, so `doctor` asks each manager once
/// for the whole module walk rather than once per declared package. The name is
/// matched through `package_identity`, exactly as the drift walk in `cli::diff`
/// does: a case-insensitive manager lists `wget` while the module declares
/// `Wget`, and a raw comparison reads an installed package as missing. Without
/// a state store there is no context, and every package reads not-installed —
/// unchanged from before the memo.
fn package_is_installed(
    cx: Option<&cfgd_core::providers::PackageContext<'_>>,
    mgr_map: &std::collections::HashMap<String, &dyn cfgd_core::providers::PackageManager>,
    manager: &str,
    resolved_name: &str,
) -> bool {
    let Some(cx) = cx else {
        return false;
    };
    mgr_map
        .get(manager)
        .and_then(|m| {
            let installed = cx.installed_for(*m).ok()?;
            Some(installed.contains(&m.package_identity(resolved_name)))
        })
        .unwrap_or(false)
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
                        path: cli.config.display().to_string(),
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
                    path: cli.config.display().to_string(),
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
                path: cli.config.display().to_string(),
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
                .status(Role::Warn, "doctor: manifest resolution failed")
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
                            "custom manager '{}' contains '.' in its name: source-delivered packages under it cannot carry decisions (the decision path grammar splits on '.') and are withheld from every run",
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
            });
        }
    }

    let module_list: Vec<String> = doctor_profile
        .as_ref()
        .map(|r| r.merged.modules.clone())
        .unwrap_or_default();

    let cache_base = module_cache_dir(cli).unwrap_or_default();
    sp.set_message("Probing: modules");
    let all_modules =
        modules::load_all_modules(&config_dir, &cache_base, &[], printer).unwrap_or_default();

    // Per-module package detail: resolve each declared package against the
    // platform's manager and query installed_packages to know whether the
    // declared state is realized.
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
    let doctor_cx = ctx
        .state_opt()
        .map(|state| cfgd_core::providers::PackageContext::new(printer, state));

    let module_checks: Vec<DoctorModuleCheck> = module_list
        .iter()
        .map(|mod_name| {
            if let Some(module) = all_modules.get(mod_name) {
                let packages: Vec<DoctorModulePackageCheck> = module
                    .spec
                    .packages
                    .iter()
                    .map(|entry| {
                        match modules::resolve_package(entry, mod_name, platform, &mgr_map) {
                            Ok(Some(mut resolved)) => {
                                // Doctor prints the version per package, so it
                                // is one of the surfaces that asks for one.
                                modules::fill_available_versions(
                                    std::slice::from_mut(&mut resolved),
                                    &mgr_map,
                                );
                                // One enumeration per manager for the whole
                                // walk: `doctor` asks about every package of
                                // every module, and the memo behind the
                                // context is what keeps that one question per
                                // manager instead of one per entry.
                                let installed = package_is_installed(
                                    doctor_cx.as_ref(),
                                    &mgr_map,
                                    &resolved.manager,
                                    &resolved.resolved_name,
                                );
                                DoctorModulePackageCheck {
                                    name: entry.name.clone(),
                                    resolved_name: resolved.resolved_name,
                                    manager: resolved.manager,
                                    installed,
                                    version: resolved.version,
                                    skip_reason: None,
                                    error: None,
                                }
                            }
                            Ok(None) => DoctorModulePackageCheck {
                                name: entry.name.clone(),
                                resolved_name: entry.name.clone(),
                                manager: String::new(),
                                installed: false,
                                version: None,
                                skip_reason: Some("platform".into()),
                                error: None,
                            },
                            Err(e) => DoctorModulePackageCheck {
                                name: entry.name.clone(),
                                resolved_name: entry.name.clone(),
                                manager: String::new(),
                                installed: false,
                                version: None,
                                skip_reason: None,
                                error: Some(e.to_string()),
                            },
                        }
                    })
                    .collect();
                DoctorModuleCheck {
                    name: mod_name.clone(),
                    valid: true,
                    error: None,
                    packages,
                }
            } else {
                DoctorModuleCheck {
                    name: mod_name.clone(),
                    valid: false,
                    error: Some(format!("module {}", cfgd_core::Absence::NotFound)),
                    packages: Vec::new(),
                }
            }
        })
        .collect();

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
        path: profiles_dir_path.display().to_string(),
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
                        Some(p.display().to_string())
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
            age_key_path: health
                .age_key_path
                .as_ref()
                .map(|p| p.display().to_string()),
            sops_config_exists: health.sops_config_exists,
            sops_config_path: health
                .sops_config_path
                .as_ref()
                .map(|p| p.display().to_string()),
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
    doc = doc.section("System", |s| build_system_section(s, extras));
    doc = doc.section_if_nonempty(
        "Config Sources",
        &extras.config_sources,
        build_sources_section,
    );

    if all_passed(output) {
        doc = doc.status(Role::Ok, "All checks passed");
    } else {
        doc = doc.status_with(Role::Fail, "Some checks failed", |f| f.detail("see above"));
    }

    doc.with_data(output)
}

fn build_config_section(s: SectionBuilder, cfg: &DoctorConfigCheck) -> SectionBuilder {
    match cfg.state {
        DoctorConfigState::Valid => {
            let mut s = s.status_with(Role::Ok, "Config file", |f| {
                f.qualifier(format!("{} (valid)", cfg.path))
            });
            let mut pairs: Vec<(String, String)> = Vec::new();
            if let Some(name) = cfg.name.as_deref() {
                pairs.push(("Name".into(), name.into()));
            }
            pairs.push((
                "Profile".into(),
                cfg.profile.as_deref().unwrap_or("(none)").into(),
            ));
            s = s.kv_block(pairs);
            s
        }
        DoctorConfigState::MissingAtDefault => s.status_with(Role::Warn, "Config file", |sf| {
            sf.qualifier(cfg.path.clone()).detail(format!(
                "{}; run 'cfgd init' to create one",
                cfgd_core::Absence::NotFound
            ))
        }),
        DoctorConfigState::MissingAtExplicit => s.status_with(Role::Fail, "Config file", |sf| {
            sf.qualifier(cfg.path.clone()).detail(format!(
                "{}; the given --config/--config-dir/CFGD_CONFIG path does not exist",
                cfgd_core::Absence::NotFound
            ))
        }),
        DoctorConfigState::Invalid => s.status_with(Role::Fail, "Config file", |f| {
            f.qualifier(cfg.path.clone())
                .detail(cfg.error.as_deref().unwrap_or("invalid").to_string())
        }),
    }
}

fn build_tools_section(s: SectionBuilder, git_available: bool) -> SectionBuilder {
    if git_available {
        s.status_with(Role::Ok, "git", |f| f.qualifier("found"))
    } else {
        s.status_with(Role::Fail, "git", |f| {
            f.qualifier(cfgd_core::Absence::NotFound.as_str())
                .detail("install git to use cfgd")
        })
    }
}

fn build_secrets_section(mut s: SectionBuilder, secrets: &DoctorSecretsCheck) -> SectionBuilder {
    s = if secrets.sops_available {
        let version_str = secrets.sops_version.as_deref().unwrap_or("unknown version");
        s.status_with(Role::Ok, "sops", |f| {
            f.qualifier(format!("found ({})", version_str))
        })
    } else {
        s.status_with(Role::Warn, "sops", |f| {
            f.qualifier(cfgd_core::Absence::NotFound.as_str())
                .detail("required for secrets (https://github.com/getsops/sops#install)")
        })
    };

    s = match (secrets.age_key_exists, secrets.age_key_path.as_deref()) {
        (true, Some(path)) => s.status_with(Role::Ok, "age key", |f| f.qualifier(path.to_string())),
        (false, Some(path)) => s.status_with(Role::Warn, "age key", |f| {
            f.qualifier(path.to_string()).detail(format!(
                "{}; run 'cfgd init' to generate",
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
            s.status_with(Role::Ok, ".sops.yaml", |f| f.qualifier(path.to_string()))
        }
        (true, None) => s.status_with(Role::Ok, ".sops.yaml", |f| f.qualifier("present")),
        (false, _) => s.status_with(Role::Warn, ".sops.yaml", |f| {
            f.qualifier(cfgd_core::Absence::NotFound.as_str())
                .detail("will be generated on 'cfgd init'")
        }),
    };

    for provider in &secrets.providers {
        s = if provider.available {
            s.status_with(Role::Ok, format!("provider {}", provider.name), |f| {
                f.qualifier("available")
            })
        } else {
            s.status_with(Role::Info, format!("provider {}", provider.name), |f| {
                f.qualifier(format!("{} (optional)", cfgd_core::Absence::NotInstalled))
            })
        };
    }
    s
}

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
            s.status_with(Role::Info, m.name.clone(), |sf| {
                sf.qualifier("available (not used in config)")
            })
        } else {
            s
        }
    })
}

fn build_modules_section(s: SectionBuilder, modules: &[DoctorModuleCheck]) -> SectionBuilder {
    modules.iter().fold(s, |s, m| {
        if !m.valid {
            let detail = m.error.clone().unwrap_or_else(|| "invalid".into());
            return s.status_with(Role::Fail, m.name.clone(), |sf| sf.detail(detail));
        }
        if m.packages.is_empty() {
            return s.status(Role::Ok, m.name.clone());
        }
        s.subsection(m.name.clone(), |sub| {
            m.packages.iter().fold(sub, build_module_package_status)
        })
    })
}

fn build_profiles_section(
    s: SectionBuilder,
    profiles: &[DoctorProfileLayoutCheck],
) -> SectionBuilder {
    if profiles.iter().all(|p| !p.legacy && p.error.is_none()) {
        return s.status(Role::Ok, "All profiles use the canonical bundle layout");
    }
    profiles.iter().fold(s, |s, p| {
        if let Some(err) = p.error.as_deref() {
            // Ambiguous / unscannable profiles are hard-broken (every load of
            // them errors), unlike the supported legacy form — Fail, not Warn.
            s.status(Role::Fail, cfgd_core::output::collapse_to_subject_line(err))
        } else if p.legacy {
            s.status_with(Role::Warn, format!("profile '{}'", p.name), |sf| {
                sf.qualifier("uses the legacy flat layout")
                    .detail(format!("run 'cfgd profile migrate {}'", p.name))
            })
        } else {
            s.status(Role::Ok, p.name.clone())
        }
    })
}

fn build_module_package_status(
    sub: SectionBuilder,
    pkg: &DoctorModulePackageCheck,
) -> SectionBuilder {
    if let Some(err) = pkg.error.as_deref() {
        return sub.status_with(Role::Fail, pkg.name.clone(), |sf| {
            sf.detail(cfgd_core::output::collapse_to_subject_line(err))
        });
    }
    if let Some(reason) = pkg.skip_reason.as_deref() {
        return sub.status_with(Role::Info, pkg.name.clone(), |sf| {
            sf.detail(format!("skipped ({})", reason))
        });
    }
    if pkg.installed {
        let ver = pkg.version.as_deref().unwrap_or("?");
        sub.status(
            Role::Ok,
            format!(
                "{} {} ({}, {})",
                pkg.name, ver, pkg.manager, pkg.resolved_name
            ),
        )
    } else {
        sub.status_with(Role::Fail, pkg.name.clone(), |sf| {
            sf.detail(format!(
                "{} ({} {})",
                cfgd_core::Absence::NotInstalled,
                pkg.manager,
                pkg.resolved_name
            ))
        })
    }
}

fn build_system_section(mut s: SectionBuilder, extras: &DoctorExtras) -> SectionBuilder {
    if let Some(ss) = extras.state_store.as_ref() {
        s = if ss.accessible {
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
                sf.qualifier(pd.path.clone())
                    .detail(cfgd_core::output::collapse_to_subject_line(err))
            })
        } else if pd.exists {
            s.status_with(Role::Ok, "Profiles directory", |sf| {
                sf.qualifier(format!("{} ({} profiles)", pd.path, pd.profile_count))
            })
        } else {
            s.status_with(
                Role::Warn,
                format!("Profiles directory {}", cfgd_core::Absence::NotFound),
                |sf| sf.qualifier(pd.path.clone()),
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
                f.qualifier(format!("cached at {}", path))
            }),
            None => s.status_with(Role::Warn, source.name.clone(), |f| {
                f.qualifier("not cached (run 'cfgd source update')")
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
                && m.packages
                    .iter()
                    .all(|p| p.error.is_none() && (p.installed || p.skip_reason.is_some()))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn mgr_map<'a>(
        managers: &'a [&'a dyn cfgd_core::providers::PackageManager],
    ) -> std::collections::HashMap<String, &'a dyn cfgd_core::providers::PackageManager> {
        managers
            .iter()
            .map(|m| (m.name().to_string(), *m))
            .collect()
    }

    // `cfgd diff` reports a chocolatey-declared `Wget` as installed because it
    // matches through `package_identity`; `doctor` compared the raw declared
    // name and reported the same package missing.
    #[test]
    fn a_case_insensitive_managers_package_reads_installed_in_doctor() {
        let choco = cfgd_core::test_helpers::MockPackageManager::new("chocolatey")
            .case_insensitive()
            .with_installed(&["wget"]);
        let managers: Vec<&dyn cfgd_core::providers::PackageManager> = vec![&choco];
        let map = mgr_map(&managers);

        let printer = cfgd_core::test_helpers::test_printer();
        let state = cfgd_core::state::StateStore::open_in_memory().unwrap();
        let cx = cfgd_core::providers::PackageContext::new(&printer, &state);

        assert!(
            package_is_installed(Some(&cx), &map, "chocolatey", "Wget"),
            "a declared `Wget` must match the listed `wget`"
        );
        assert!(
            !package_is_installed(Some(&cx), &map, "chocolatey", "ripgrep"),
            "a genuinely absent package must still read not installed"
        );
    }

    #[test]
    fn every_package_reads_not_installed_without_a_state_store() {
        let apt = cfgd_core::test_helpers::MockPackageManager::new("apt").with_installed(&["curl"]);
        let managers: Vec<&dyn cfgd_core::providers::PackageManager> = vec![&apt];
        assert!(!package_is_installed(
            None,
            &mgr_map(&managers),
            "apt",
            "curl"
        ));
    }

    // One question per manager for the whole module walk, however many packages
    // the modules declare under it.
    #[test]
    fn doctor_asks_each_manager_once_for_the_whole_walk() {
        // The count is a memo-hit claim, so the memo's age ceiling is pinned out
        // of reach — unpinned it rests on the 30s wall clock. No serialization:
        // nothing in this crate's test binary pins the ceiling to zero, and a
        // longer ceiling can only let another test's entries live longer.
        let _ttl = cfgd_core::test_helpers::EnumerationMemoTtlGuard::never_expires();
        let enumerations = cfgd_core::test_helpers::measured_in_a_stable_generation(|| {
            let apt = cfgd_core::test_helpers::MockPackageManager::new("apt")
                .with_installed(&["curl", "jq"]);
            let counter = apt.enumeration_counter();
            let managers: Vec<&dyn cfgd_core::providers::PackageManager> = vec![&apt];
            let map = mgr_map(&managers);

            let printer = cfgd_core::test_helpers::test_printer();
            let state = cfgd_core::state::StateStore::open_in_memory().unwrap();
            let cx = cfgd_core::providers::PackageContext::new(&printer, &state);

            for name in ["curl", "jq", "ripgrep", "fd"] {
                package_is_installed(Some(&cx), &map, "apt", name);
            }

            counter.load(std::sync::atomic::Ordering::SeqCst)
        });

        assert_eq!(enumerations, 1);
    }
}
