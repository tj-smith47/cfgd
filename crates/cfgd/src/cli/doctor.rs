use super::*;
use cfgd_core::output::{Doc, Printer, Role, doc::SectionBuilder};

pub(super) fn cmd_doctor(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    let (output, extras) = collect_doctor_output(cli, printer)?;
    printer.emit(build_doctor_doc(&output, &extras));
    Ok(())
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
}

pub struct DoctorStateStore {
    pub accessible: bool,
    pub message: Option<String>,
}

pub struct DoctorProfilesDir {
    pub path: String,
    pub exists: bool,
    pub profile_count: usize,
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
) -> anyhow::Result<(DoctorOutput, DoctorExtras)> {
    let (config_check, loaded_cfg) = if cli.config.exists() {
        match config::load_config(&cli.config) {
            Ok(cfg) => (
                DoctorConfigCheck {
                    valid: true,
                    path: cli.config.display().to_string(),
                    name: Some(cfg.metadata.name.clone()),
                    profile: cfg.spec.profile.clone(),
                    error: None,
                },
                Some(cfg),
            ),
            Err(e) => (
                DoctorConfigCheck {
                    valid: false,
                    path: cli.config.display().to_string(),
                    name: None,
                    profile: None,
                    error: Some(format!("{}", e)),
                },
                None,
            ),
        }
    } else {
        (
            DoctorConfigCheck {
                valid: false,
                path: cli.config.display().to_string(),
                name: None,
                profile: None,
                error: Some("not found".into()),
            },
            None,
        )
    };

    let git_available = cfgd_core::command_available("git");

    let config_dir = config_dir(cli);
    let age_key_override = loaded_cfg
        .as_ref()
        .and_then(|c| c.spec.secrets.as_ref())
        .and_then(|s| s.sops.as_ref())
        .and_then(|s| s.age_key.as_ref());

    let health = secrets::check_secrets_health(&config_dir, age_key_override.map(|p| p.as_path()));

    let resolved_packages = if let Some(ref cfg) = loaded_cfg {
        let profiles_dir = profiles_dir(cli);
        let profile_name = cli.profile.as_deref().or(cfg.spec.profile.as_deref());
        if let Some(pn) = profile_name
            && let Ok(mut resolved) = config::resolve_profile(pn, &profiles_dir)
        {
            let _ = packages::resolve_manifest_packages(&mut resolved.merged.packages, &config_dir);
            Some(resolved.merged.packages)
        } else {
            None
        }
    } else {
        None
    };

    let registry = if let Some(ref pkgs) = resolved_packages {
        build_registry_with_profile(pkgs)
    } else {
        build_registry()
    };
    let all_managers = &registry.package_managers;

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
            let can_bootstrap = mgr.can_bootstrap();
            let bootstrap_method = if can_bootstrap {
                Some(packages::bootstrap_method(mgr.as_ref()).to_string())
            } else {
                None
            };
            manager_checks.push(DoctorManagerCheck {
                name: name.to_string(),
                available: mgr.is_available(),
                declared: declared_managers.iter().any(|d| d == name),
                can_bootstrap,
                bootstrap_method,
            });
        }
    }

    let module_list: Vec<String> = if let Some(ref cfg) = loaded_cfg {
        let profiles_dir = profiles_dir(cli);
        let profile_name = cli.profile.as_deref().or(cfg.spec.profile.as_deref());
        profile_name
            .and_then(|pn| config::resolve_profile(pn, &profiles_dir).ok())
            .map(|r| r.merged.modules)
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let cache_base = modules::default_module_cache_dir().unwrap_or_default();
    let all_modules =
        modules::load_all_modules(&config_dir, &cache_base, printer).unwrap_or_default();

    // Per-module package detail: resolve each declared package against the
    // platform's manager and query installed_packages to know whether the
    // declared state is realized. The "modules-only" registry mirrors what
    // `cfgd apply` would use for the install path.
    let modules_registry = build_registry();
    let mgr_map = managers_map(&modules_registry);
    let platform = Platform::detect();

    let module_checks: Vec<DoctorModuleCheck> = module_list
        .iter()
        .map(|mod_name| {
            if let Some(module) = all_modules.get(mod_name) {
                let packages: Vec<DoctorModulePackageCheck> = module
                    .spec
                    .packages
                    .iter()
                    .map(|entry| {
                        match modules::resolve_package(entry, mod_name, &platform, &mgr_map) {
                            Ok(Some(resolved)) => {
                                let installed = mgr_map
                                    .get(&resolved.manager)
                                    .and_then(|m| m.installed_packages().ok())
                                    .map(|pkgs| pkgs.contains(&resolved.resolved_name))
                                    .unwrap_or(false);
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
                    error: Some("module not found".into()),
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

    let state_store = match StateStore::open_default() {
        Ok(_) => DoctorStateStore {
            accessible: true,
            message: None,
        },
        Err(e) => DoctorStateStore {
            accessible: false,
            message: Some(e.to_string()),
        },
    };

    let profiles_dir_path = profiles_dir(cli);
    let profiles_dir_extra = if profiles_dir_path.exists() {
        let count = std::fs::read_dir(&profiles_dir_path)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter(|e| cfgd_core::config::is_yaml_ext(&e.path()))
                    .count()
            })
            .unwrap_or(0);
        DoctorProfilesDir {
            path: profiles_dir_path.display().to_string(),
            exists: true,
            profile_count: count,
        }
    } else {
        DoctorProfilesDir {
            path: profiles_dir_path.display().to_string(),
            exists: false,
            profile_count: 0,
        }
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
    };

    let extras = DoctorExtras {
        state_store: Some(state_store),
        profiles_dir: Some(profiles_dir_extra),
        config_sources,
    };

    Ok((output, extras))
}

/// Build the doctor `Doc` from a collected payload + display-only extras. Used
/// by the live command and by snapshot tests under
/// `tests/output_snapshots/doctor/`.
pub fn build_doctor_doc(output: &DoctorOutput, extras: &DoctorExtras) -> Doc {
    let mut doc = Doc::new().heading("Doctor");

    // Config emits at top-level (Status then KVs) rather than nested in a
    // section because a section's pending_statuses buffer flushes Status
    // lines after KVs, inverting the intended order.
    doc = build_config_top(doc, &output.config);
    doc = doc.section("Tools", |s| build_tools_section(s, output.git));
    doc = doc.section("Secrets", |s| build_secrets_section(s, &output.secrets));
    doc = doc.section_if_nonempty(
        "Package Managers",
        &output.package_managers,
        build_managers_section,
    );
    doc = doc.section_if_nonempty("Modules", &output.modules, build_modules_section);
    doc = doc.section("System", |s| build_system_section(s, extras));
    doc = doc.section_if_nonempty(
        "Config Sources",
        &extras.config_sources,
        build_sources_section,
    );

    if all_passed(output) {
        doc = doc.status(Role::Ok, "All checks passed");
    } else {
        doc = doc.status(Role::Fail, "Some checks failed — see above");
    }

    doc.with_data(output)
}

fn build_config_top(doc: Doc, cfg: &DoctorConfigCheck) -> Doc {
    if cfg.valid {
        let mut doc = doc.status(Role::Ok, format!("Config file: {} (valid)", cfg.path));
        let mut pairs: Vec<(String, String)> = Vec::new();
        if let Some(name) = cfg.name.as_deref() {
            pairs.push(("Name".into(), name.into()));
        }
        pairs.push((
            "Profile".into(),
            cfg.profile.as_deref().unwrap_or("(none)").into(),
        ));
        doc = doc.kv_block(pairs);
        doc
    } else if let Some(err) = cfg.error.as_deref() {
        if err == "not found" {
            doc.status_with(
                Role::Warn,
                format!("Config file: {} — not found", cfg.path),
                |sf| sf.detail("run 'cfgd init' to create one"),
            )
        } else {
            doc.status(Role::Fail, format!("Config file: {} — {}", cfg.path, err))
        }
    } else {
        doc.status(Role::Fail, "Config file: invalid")
    }
}

fn build_tools_section(s: SectionBuilder, git_available: bool) -> SectionBuilder {
    if git_available {
        s.status(Role::Ok, "git: found")
    } else {
        s.status(Role::Fail, "git: not found — install git to use cfgd")
    }
}

fn build_secrets_section(mut s: SectionBuilder, secrets: &DoctorSecretsCheck) -> SectionBuilder {
    s = if secrets.sops_available {
        let version_str = secrets.sops_version.as_deref().unwrap_or("unknown version");
        s.status(Role::Ok, format!("sops: found ({})", version_str))
    } else {
        s.status(
            Role::Warn,
            "sops: not found — required for secrets (https://github.com/getsops/sops#install)",
        )
    };

    s = match (secrets.age_key_exists, secrets.age_key_path.as_deref()) {
        (true, Some(path)) => s.status(Role::Ok, format!("age key: {}", path)),
        (false, Some(path)) => s.status(
            Role::Warn,
            format!(
                "age key: not found at {} — run 'cfgd init' to generate",
                path
            ),
        ),
        _ => s,
    };

    s = match (
        secrets.sops_config_exists,
        secrets.sops_config_path.as_deref(),
    ) {
        (true, Some(path)) => s.status(Role::Ok, format!(".sops.yaml: {}", path)),
        (true, None) => s.status(Role::Ok, ".sops.yaml: present"),
        (false, _) => s.status(
            Role::Warn,
            ".sops.yaml: not found — will be generated on 'cfgd init'",
        ),
    };

    for provider in &secrets.providers {
        s = if provider.available {
            s.status(Role::Ok, format!("provider {}: available", provider.name))
        } else {
            s.status(
                Role::Info,
                format!("provider {}: not installed (optional)", provider.name),
            )
        };
    }
    s
}

fn build_managers_section(s: SectionBuilder, managers: &[DoctorManagerCheck]) -> SectionBuilder {
    managers.iter().fold(s, |s, m| {
        if m.declared {
            if m.available {
                s.status(
                    Role::Ok,
                    format!("{}: available (declared in config)", m.name),
                )
            } else if m.can_bootstrap {
                let detail = match m.bootstrap_method.as_deref() {
                    Some(method) => format!("can auto-bootstrap via {}", method),
                    None => "can auto-bootstrap".into(),
                };
                s.status_with(Role::Warn, format!("{}: not found", m.name), |sf| {
                    sf.detail(detail)
                })
            } else {
                s.status(
                    Role::Fail,
                    format!(
                        "{}: not found — declared in config but not available",
                        m.name
                    ),
                )
            }
        } else if m.available {
            s.status(
                Role::Info,
                format!("{}: available (not used in config)", m.name),
            )
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

fn build_module_package_status(
    sub: SectionBuilder,
    pkg: &DoctorModulePackageCheck,
) -> SectionBuilder {
    if let Some(err) = pkg.error.as_deref() {
        return sub.status_with(Role::Fail, pkg.name.clone(), |sf| {
            sf.detail(err.to_string())
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
                "not installed ({} {})",
                pkg.manager, pkg.resolved_name
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
        s = if pd.exists {
            s.status(
                Role::Ok,
                format!(
                    "Profiles directory: {} ({} profiles)",
                    pd.path, pd.profile_count
                ),
            )
        } else {
            s.status(
                Role::Warn,
                format!("Profiles directory not found: {}", pd.path),
            )
        };
    }
    s
}

fn build_sources_section(s: SectionBuilder, sources: &[DoctorConfigSource]) -> SectionBuilder {
    sources
        .iter()
        .fold(s, |s, source| match source.cached_path.as_deref() {
            Some(path) => s.status(Role::Ok, format!("{}: cached at {}", source.name, path)),
            None => s.status(
                Role::Warn,
                format!("{}: not cached (run 'cfgd source update')", source.name),
            ),
        })
}

fn all_passed(output: &DoctorOutput) -> bool {
    output.config.valid
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
}
