use super::*;
use cfgd_core::PathDisplayExt;
use cfgd_core::config::ModuleLockEntry;
use cfgd_core::output::{Doc, KvPair, Printer, Role, renderer::Table};

/// Per-package display row for `cfgd module show`. Computed from package
/// resolution so the renderer is pure and snapshot-testable without needing a
/// live `ProviderRegistry` or `Platform`.
pub enum PackageDisplay {
    Resolved {
        name: String,
        manager: String,
        resolved_name: String,
        version: Option<String>,
    },
    Skipped {
        name: String,
        platforms: String,
    },
    Unresolved {
        summary: String,
        error: String,
    },
}

/// A declared value with its own `platforms:` gate named after it, for the two
/// surfaces that list a module's DOCUMENT rather than this host's desired
/// state. Ungated values are returned untouched.
pub(in crate::cli) fn gated_value(
    value: String,
    entry: &impl cfgd_core::platform::PlatformGated,
) -> String {
    match entry.platform_annotation() {
        Some(tags) => format!("{value} ({tags})"),
        None => value,
    }
}

/// `secondary` (pink/magenta) attaches to remote-sourced modules so the
/// upgrade-candidate set is scannable without re-reading the column. The
/// literal value ("remote") still carries the meaning when colors are off.
fn source_role(source: &str) -> Option<Role> {
    (source == "remote").then_some(Role::Secondary)
}

/// The Status cell: the display word and its tint, both from the workspace's
/// one module-state vocabulary. No row here can read `Drifted` — that verdict
/// comes from a live scan, and `module list` reports recorded state only.
fn status_cell(status: &str) -> (String, Option<Role>) {
    let (word, role) = cfgd_core::state::module_status_display(status, false);
    (word.to_string(), Some(role))
}

/// Build the `cfgd module list` Doc. Caller owns `entries` (constructed from
/// disk + state); this fn is pure.
pub fn build_module_list_doc(entries: &[ModuleListEntry], wide: bool, config_dir: &Path) -> Doc {
    let mut doc = Doc::new().heading("Modules");

    if entries.is_empty() {
        doc = doc.status(Role::Info, "No modules found").hint(format!(
            "Create one with `cfgd module create <name>`, or add a directory under {}/modules/",
            config_dir.posix()
        ));
        return doc.with_data(entries);
    }

    let table = if wide {
        let mut t = Table::new([
            "Module", "Active", "Source", "Status", "Packages", "Files", "Deps",
        ]);
        for e in entries {
            t = t.row_styled([
                (e.name.clone(), None),
                (cfgd_core::yes_no(Some(e.active)).to_string(), None),
                (e.source.clone(), source_role(&e.source)),
                status_cell(&e.status),
                (e.packages.to_string(), None),
                (e.files.to_string(), None),
                (e.depends.to_string(), None),
            ]);
        }
        t
    } else {
        let mut t = Table::new(["Module", "Active", "Source", "Status", "Contents"]);
        for e in entries {
            t = t.row_styled([
                (e.name.clone(), None),
                (cfgd_core::yes_no(Some(e.active)).to_string(), None),
                (e.source.clone(), source_role(&e.source)),
                status_cell(&e.status),
                (
                    format!(
                        "{}, {}, {}",
                        cfgd_core::pluralize(e.packages, "package"),
                        cfgd_core::pluralize(e.files, "file"),
                        cfgd_core::pluralize(e.depends, "dep")
                    ),
                    None,
                ),
            ]);
        }
        t
    };

    doc.table(table.without_unfillable_columns())
        .with_data(entries)
}

/// Build the not-found error returned to `main.rs::render_cli_error`, the sole
/// error sink. Carries the structured `not_found` payload (the available-module
/// list) for `-o json` consumers and a human-mode hint listing the available
/// modules; the sink renders the `✗` line and the hint exactly once.
pub fn build_module_not_found_error(name: &str, available: &[String]) -> anyhow::Error {
    let mut hints = Vec::new();
    if !available.is_empty() {
        hints.push(format!("Available modules: {}", available.join(", ")));
    }
    // Carry the typed `ModuleError::NotFound` in the chain so the exit-code
    // downcast in `main.rs` resolves to ExitCode::NotFound (6); the attached
    // CliErrorMeta still drives the rich `not_found` payload + hints.
    crate::cli::cli_error_ctx_with_hints(
        cfgd_core::errors::CfgdError::Module(cfgd_core::errors::ModuleError::NotFound {
            name: name.to_string(),
        })
        .into(),
        name,
        "not_found",
        format!("Module '{}' not found", name),
        serde_json::json!({ "available": available }),
        hints,
    )
}

/// Build the `cfgd module show` Doc from precomputed inputs.
pub fn build_module_show_doc(
    output: &ModuleShowOutput,
    lock_entry: Option<&ModuleLockEntry>,
    packages: &[PackageDisplay],
    show_values: bool,
    arrow: &str,
    now: &str,
) -> Doc {
    // One aligned block: the Status row needs a role-tinted value, which only
    // `kv_rows` can carry, and `kv_rows` does not coalesce with a preceding
    // `kv` block — so every row of the header is built here.
    let mut rows = Vec::new();
    if let Some(version) = &output.metadata.version {
        rows.push(KvPair::new("Version", version));
    }
    if !output.depends.is_empty() {
        rows.push(KvPair::new("Dependencies", output.depends.join(", ")));
    }
    rows.push(KvPair::new(
        "Directory",
        cfgd_core::fold_home_in_text(&output.directory),
    ));

    if let Some(entry) = lock_entry {
        rows.push(KvPair::annotated("Source", "remote", "locked"));
        rows.push(KvPair::new("URL", &entry.url));
        rows.push(KvPair::new("Pinned Ref", &entry.pinned_ref));
        rows.push(KvPair::new("Commit", &entry.commit));
        rows.push(KvPair::new("Integrity", &entry.integrity));
    } else {
        rows.push(KvPair::new("Source", "local"));
    }

    if let Some(state_rec) = &output.state {
        // Recorded state only, same as the list table — see `status_cell`.
        let (word, role) = cfgd_core::state::module_status_display(&state_rec.status, false);
        rows.push(KvPair::role_valued("Status", word, role));
        // The age, not the recorded instant: `-o json`'s `state.installedAt`
        // carries the exact moment, and the row a person reads answers how
        // long ago — the same split `cfgd status <module>` makes.
        rows.push(KvPair::new(
            "Last Applied",
            cfgd_core::humanize_age_cell(Some(&state_rec.installed_at), now),
        ));
        rows.push(KvPair::new("Packages Hash", &state_rec.packages_hash));
        rows.push(KvPair::new("Files Hash", &state_rec.files_hash));
    }

    let mut doc = Doc::new()
        .heading_title("Module", &output.name)
        .kv_rows(rows);

    doc = doc.section_if_nonempty("Packages", packages, |s, pkgs| {
        pkgs.iter().fold(s, |s, pkg| match pkg {
            PackageDisplay::Resolved {
                name,
                manager,
                resolved_name,
                version,
            } => {
                let ver = version
                    .as_ref()
                    .map(|v| format!(" ({})", v))
                    .unwrap_or_default();
                s.status(
                    Role::Ok,
                    format!(
                        "{} {} {} install {}{}",
                        name, arrow, manager, resolved_name, ver
                    ),
                )
            }
            PackageDisplay::Skipped { name, platforms } => {
                s.status_with(Role::Info, format!("{}{}", name, platforms), |f| {
                    f.detail(crate::cli::status::PLATFORM_SKIPPED)
                })
            }
            PackageDisplay::Unresolved { summary, error } => {
                // `summary` already carries two data colons of its own
                // (`prefer:`, `min:`) — `.qualifier("unresolved")` would add
                // a third with a different meaning ("unresolved" is not a
                // field on the summary). It opens the DETAIL instead, where
                // the word governs the reason that follows it; appended to the
                // subject it read as a qualifier on the last field's value
                // (`min: 1.0 unresolved`).
                s.status_with(Role::Warn, summary.clone(), |f| {
                    f.detail(format!("unresolved: {error}"))
                })
            }
        })
    });

    doc = doc.section_if_nonempty("Files", &output.spec.files, |s, files| {
        files.iter().fold(s, |s, file| {
            let git_indicator = if modules::is_git_source(&file.source) {
                " (git)"
            } else {
                ""
            };
            s.kv(format!("{}{}", file.source, git_indicator), &file.target)
        })
    });

    // This surface describes what the module DECLARES, not what this host will
    // take from it, so a gated entry is listed and annotated — the same
    // annotation vocabulary a platform-filtered package row already carries.
    // Aliases lead the pair, the order every surface naming both renders them
    // in.
    doc = doc.section_if_nonempty("Aliases", &output.spec.aliases, |s, aliases| {
        aliases.iter().fold(s, |s, alias| {
            s.kv(&alias.name, gated_value(alias.command.clone(), alias))
        })
    });

    doc = doc.section_if_nonempty("Env", &output.spec.env, |s, env| {
        env.iter().fold(s, |s, ev| {
            let display = if show_values {
                ev.value.clone()
            } else {
                mask_value(&ev.value)
            };
            s.kv(&ev.name, gated_value(display, ev))
        })
    });

    // Every hook the module declares, in execution order — read through the
    // one tally so this section and `cfgd status <module>`'s cannot disagree
    // about what the module declares. No drift engine ever watches a hook
    // body, so this is always a bare declaration, the same `command_list`
    // shape `cfgd status <module>`'s Scripts section uses: the hook name is
    // the key, never a `status` row borrowing a verdict no check gave it.
    let declared = cfgd_core::modules::ModuleSurfaces::of(&output.spec);
    doc = doc.section_if_nonempty("Scripts", &declared.scripts, |s, hooks| {
        let pairs: Vec<(String, String)> = hooks
            .iter()
            .flat_map(|hook| hook.bodies.iter().map(move |body| (hook.hook, body)))
            .map(|(hook, body)| {
                // `--show-values` is the only way to read a whole body; the
                // default row condenses it, exactly as the status inventory does.
                let value = if show_values {
                    body.clone()
                } else {
                    cfgd_core::output::condense_script_label(body)
                };
                (hook.to_string(), value)
            })
            .collect();
        s.command_list(pairs)
    });

    doc.with_data(output)
}

pub(crate) fn cmd_module_list(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    let config_dir = config_dir(cli);
    let cache_base = module_cache_dir(cli)?;
    let all_modules = modules::load_all_modules(&config_dir, &cache_base, &[], printer)?;
    let lockfile = modules::load_lockfile(&config_dir)?;

    if all_modules.is_empty() {
        printer.emit(build_module_list_doc(&[], printer.is_wide(), &config_dir));
        return Ok(());
    }

    let active_modules: Vec<String> = if cli.config.exists() {
        let (_, _, resolved) = helpers::load_config_and_profile(cli, printer)?;
        resolved.merged.modules
    } else {
        Vec::new()
    };

    let state = open_state_store(cli.state_dir.as_deref(), cli.scope())?;
    let state_map = module_state_map(&state);

    let mut names: Vec<String> = all_modules.keys().cloned().collect();
    names.sort();

    let entries: Vec<ModuleListEntry> = names
        .iter()
        .map(|name| {
            let module = &all_modules[name];
            let in_profile = active_modules
                .iter()
                .any(|r| modules::resolve_profile_module_name(r) == name);
            let status = if let Some(state_rec) = state_map.get(name) {
                state_rec.status.clone()
            } else if in_profile {
                "pending".to_string()
            } else {
                "available".to_string()
            };
            let source_type = if lockfile.modules.iter().any(|e| e.name == *name) {
                "remote"
            } else {
                "local"
            };
            ModuleListEntry {
                name: name.clone(),
                active: in_profile,
                source: source_type.to_string(),
                status,
                packages: module.spec.packages.len(),
                files: module.spec.files.len(),
                depends: module.spec.depends.len(),
            }
        })
        .collect();

    printer.emit(build_module_list_doc(
        &entries,
        printer.is_wide(),
        &config_dir,
    ));
    Ok(())
}

pub(crate) fn cmd_module_show(
    cli: &Cli,
    printer: &Printer,
    name: &str,
    show_values: bool,
) -> anyhow::Result<()> {
    let config_dir = config_dir(cli);

    // Showing a LOCAL module must not drag every locked remote module through a
    // git fetch: `load_all_modules` clones each lockfile entry, so one private
    // module without a usable credential would fail a read of an unrelated
    // local one. Local modules win the merge either way, so a local hit here is
    // the same module the full load would have returned.
    let local_modules = modules::load_modules(&config_dir)?;
    let all_modules = if local_modules.contains_key(name) {
        local_modules
    } else {
        let cache_base = module_cache_dir(cli)?;
        modules::load_all_modules(&config_dir, &cache_base, &[], printer)?
    };

    let module = match all_modules.get(name) {
        Some(m) => m,
        None => {
            let mut available: Vec<String> = all_modules.keys().map(|s| s.to_string()).collect();
            available.sort();
            return Err(build_module_not_found_error(name, &available));
        }
    };

    let lockfile = modules::load_lockfile(&config_dir)?;
    let lock_entry = lockfile.modules.iter().find(|e| e.name == name);
    let source_type = if lock_entry.is_some() {
        "remote"
    } else {
        "local"
    };

    let state = open_state_store(cli.state_dir.as_deref(), cli.scope())?;
    let state_rec = state.module_state_by_name(name)?;

    let output = ModuleShowOutput {
        name: name.to_string(),
        metadata: ModuleShowMetadata {
            version: module.version.clone(),
        },
        directory: cfgd_core::to_posix_string(&module.dir),
        source: source_type.to_string(),
        depends: module.spec.depends.clone(),
        state: state_rec,
        spec: module.spec.clone(),
    };

    // Which manager already holds a bare entry is part of what "resolved"
    // means, so the display reads the same installed state the plan does.
    let pkg_cx = cfgd_core::providers::PackageContext::new(printer, &state);
    let installed = Some(&pkg_cx);
    let packages: Vec<PackageDisplay> = if module.spec.packages.is_empty() {
        Vec::new()
    } else {
        let registry = build_registry();
        let mgr_map = registry.manager_map();
        let platform = Platform::current();
        module
            .spec
            .packages
            .iter()
            .map(|entry| {
                let prefer_str = if entry.prefer.is_empty() {
                    String::new()
                } else {
                    format!(" (prefer: {})", entry.prefer.join(", "))
                };
                let version_str = entry
                    .min_version
                    .as_ref()
                    .map(|v| format!(", min: {}", v))
                    .unwrap_or_default();
                let alias_str = if entry.aliases.is_empty() {
                    String::new()
                } else {
                    let aliases: Vec<String> = entry
                        .aliases
                        .iter()
                        .map(|(k, v)| format!("{}={}", k, v))
                        .collect();
                    format!(", aliases: {}", aliases.join(", "))
                };
                let platform_str = if entry.platforms.is_empty() {
                    String::new()
                } else {
                    format!(", platforms: {}", entry.platforms.join("/"))
                };

                match modules::resolve_package(entry, name, platform, &mgr_map, installed) {
                    Ok(Some(mut resolved)) => {
                        // `module show` prints the version beside each package,
                        // so it is one of the surfaces that asks for one.
                        modules::fill_available_versions(
                            std::slice::from_mut(&mut resolved),
                            &mgr_map,
                        );
                        PackageDisplay::Resolved {
                            name: entry.name.clone(),
                            manager: resolved.manager.clone(),
                            resolved_name: resolved.resolved_name.clone(),
                            version: resolved.version.clone(),
                        }
                    }
                    Ok(None) => PackageDisplay::Skipped {
                        name: entry.name.clone(),
                        platforms: platform_str,
                    },
                    Err(e) => PackageDisplay::Unresolved {
                        summary: format!(
                            "{}{}{}{}{}",
                            entry.name, prefer_str, version_str, alias_str, platform_str
                        ),
                        error: e.to_string(),
                    },
                }
            })
            .collect()
    };

    printer.emit(build_module_show_doc(
        &output,
        lock_entry,
        &packages,
        show_values,
        printer.arrow(),
        &cfgd_core::utc_now_iso8601(),
    ));
    Ok(())
}

#[cfg(test)]
mod role_mapping_tests {
    use super::*;

    #[test]
    fn source_role_pinks_remote_only() {
        assert_eq!(source_role("remote"), Some(Role::Secondary));
        assert_eq!(source_role("local"), None);
        assert_eq!(source_role(""), None);
        assert_eq!(source_role("registry:foo"), None);
    }

    /// The Status cell speaks the workspace's one module-state vocabulary, and
    /// the two states `module list` derives for a module with no recorded
    /// apply (`pending` / `available`) both read `NotApplied` — the row's
    /// `Active` column is what distinguishes them.
    #[test]
    fn status_cell_speaks_the_display_vocabulary() {
        assert_eq!(
            status_cell("installed"),
            ("Synced".to_string(), Some(Role::Ok))
        );
        assert_eq!(
            status_cell("error"),
            ("Failed".to_string(), Some(Role::Fail))
        );
        for no_record in ["pending", "available", ""] {
            assert_eq!(
                status_cell(no_record),
                ("NotApplied".to_string(), Some(Role::Pending)),
                "{no_record:?} should read NotApplied"
            );
        }
    }
}

// --- Module CRUD helpers ---
