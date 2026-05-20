use super::*;
use cfgd_core::config::ModuleLockEntry;
use cfgd_core::output::{Doc, Printer, Role, renderer::Table};

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

/// `secondary` (pink/magenta) attaches to remote-sourced modules so the
/// upgrade-candidate set is scannable without re-reading the column. The
/// literal value ("remote") still carries the meaning when colors are off.
fn source_role(source: &str) -> Option<Role> {
    (source == "remote").then_some(Role::Secondary)
}

/// `accent` (orange) tags rows that need a user action — currently `pending`
/// (referenced in profile but not applied) and `out-of-date` (state drift
/// detected). Other states stay plain so the call-to-action stays scannable.
fn status_role(status: &str) -> Option<Role> {
    matches!(status, "pending" | "out-of-date").then_some(Role::Accent)
}

/// Build the `cfgd module list` Doc. Caller owns `entries` (constructed from
/// disk + state); this fn is pure.
pub fn build_module_list_doc(entries: &[ModuleListEntry], wide: bool, config_dir: &Path) -> Doc {
    let mut doc = Doc::new().heading("Modules");

    if entries.is_empty() {
        doc = doc
            .status(Role::Info, "No modules found")
            .hint(format!("Add modules to {}/modules/", config_dir.display()));
        return doc.with_data(entries);
    }

    let table = if wide {
        let mut t = Table::new([
            "Module", "Active", "Source", "Status", "Packages", "Files", "Deps",
        ]);
        for e in entries {
            t = t.row_styled([
                (e.name.clone(), None),
                (if e.active { "yes" } else { "-" }.to_string(), None),
                (e.source.clone(), source_role(&e.source)),
                (e.status.clone(), status_role(&e.status)),
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
                (if e.active { "yes" } else { "-" }.to_string(), None),
                (e.source.clone(), source_role(&e.source)),
                (e.status.clone(), status_role(&e.status)),
                (
                    format!("{} pkgs, {} files, {} deps", e.packages, e.files, e.depends),
                    None,
                ),
            ]);
        }
        t
    };

    doc.table(table).with_data(entries)
}

/// Doc emitted before the not-found error bubbles to `main.rs::printer.error`.
/// Carries the structured payload for `-o json` consumers and a hint listing
/// available modules; the user-visible error string itself is rendered by
/// `main.rs` so it appears exactly once.
pub fn build_module_not_found_doc(name: &str, available: &[String]) -> Doc {
    let mut doc = Doc::new();
    if !available.is_empty() {
        doc = doc.hint(format!("Available modules: {}", available.join(", ")));
    }
    doc.with_data(serde_json::json!({
        "error": "not_found",
        "name": name,
        "available": available,
    }))
}

/// Build the `cfgd module show` Doc from precomputed inputs.
pub fn build_module_show_doc(
    output: &ModuleShowOutput,
    lock_entry: Option<&ModuleLockEntry>,
    packages: &[PackageDisplay],
    post_apply: &[String],
    show_values: bool,
) -> Doc {
    let mut doc = Doc::new().heading(format!("Module: {}", output.name));

    if !output.depends.is_empty() {
        doc = doc.kv("Dependencies", output.depends.join(", "));
    }
    doc = doc.kv("Directory", &output.directory);

    if let Some(entry) = lock_entry {
        doc = doc
            .kv("Source", "remote (locked)")
            .kv("URL", &entry.url)
            .kv("Pinned ref", &entry.pinned_ref)
            .kv("Commit", &entry.commit)
            .kv("Integrity", &entry.integrity);
    } else {
        doc = doc.kv("Source", "local");
    }

    if let Some(state_rec) = &output.state {
        doc = doc
            .kv("Status", &state_rec.status)
            .kv("Last applied", &state_rec.installed_at)
            .kv("Packages hash", &state_rec.packages_hash)
            .kv("Files hash", &state_rec.files_hash);
    }

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
                    format!("{} -> {} install {}{}", name, manager, resolved_name, ver),
                )
            }
            PackageDisplay::Skipped { name, platforms } => s.status(
                Role::Info,
                format!("{}{} — skipped (platform filter)", name, platforms),
            ),
            PackageDisplay::Unresolved { summary, error } => {
                s.status(Role::Warn, format!("{} — unresolved: {}", summary, error))
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

    doc = doc.section_if_nonempty("Env", &output.spec.env, |s, env| {
        env.iter().fold(s, |s, ev| {
            let display = if show_values {
                ev.value.clone()
            } else {
                mask_value(&ev.value)
            };
            s.kv(&ev.name, display)
        })
    });

    doc = doc.section_if_nonempty("Aliases", &output.spec.aliases, |s, aliases| {
        aliases
            .iter()
            .fold(s, |s, alias| s.kv(&alias.name, &alias.command))
    });

    doc = doc.section_if_nonempty("Post-apply Scripts", post_apply, |s, scripts| {
        scripts
            .iter()
            .fold(s, |s, script| s.status(Role::Info, script))
    });

    doc.with_data(output)
}

pub(crate) fn cmd_module_list(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    let config_dir = config_dir(cli);
    let cache_base = modules::default_module_cache_dir()?;
    let all_modules = modules::load_all_modules(&config_dir, &cache_base, printer)?;
    let lockfile = modules::load_lockfile(&config_dir)?;

    if all_modules.is_empty() {
        printer.emit(build_module_list_doc(&[], printer.is_wide(), &config_dir));
        return Ok(());
    }

    let active_modules: Vec<String> = if cli.config.exists() {
        let (_, _, resolved) = helpers::load_config_and_profile(cli)?;
        resolved.merged.modules
    } else {
        Vec::new()
    };

    let state = open_state_store(cli.state_dir.as_deref())?;
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
    let cache_base = modules::default_module_cache_dir()?;
    let all_modules = modules::load_all_modules(&config_dir, &cache_base, printer)?;

    let module = match all_modules.get(name) {
        Some(m) => m,
        None => {
            let mut available: Vec<String> = all_modules.keys().map(|s| s.to_string()).collect();
            available.sort();
            printer.emit(build_module_not_found_doc(name, &available));
            anyhow::bail!("Module '{}' not found", name);
        }
    };

    let lockfile = modules::load_lockfile(&config_dir)?;
    let lock_entry = lockfile.modules.iter().find(|e| e.name == name);
    let source_type = if lock_entry.is_some() {
        "remote"
    } else {
        "local"
    };

    let state = open_state_store(cli.state_dir.as_deref())?;
    let state_rec = state.module_state_by_name(name)?;

    let output = ModuleShowOutput {
        name: name.to_string(),
        directory: module.dir.display().to_string(),
        source: source_type.to_string(),
        depends: module.spec.depends.clone(),
        state: state_rec,
        spec: module.spec.clone(),
    };

    let packages: Vec<PackageDisplay> = if module.spec.packages.is_empty() {
        Vec::new()
    } else {
        let registry = build_registry();
        let mgr_map = managers_map(&registry);
        let platform = Platform::detect();
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

                match modules::resolve_package(entry, name, &platform, &mgr_map) {
                    Ok(Some(resolved)) => PackageDisplay::Resolved {
                        name: entry.name.clone(),
                        manager: resolved.manager.clone(),
                        resolved_name: resolved.resolved_name.clone(),
                        version: resolved.version.clone(),
                    },
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

    let post_apply: Vec<String> = module
        .spec
        .scripts
        .as_ref()
        .map(|s| {
            s.post_apply
                .iter()
                .map(|e| e.run_str().to_string())
                .collect()
        })
        .unwrap_or_default();

    printer.emit(build_module_show_doc(
        &output,
        lock_entry,
        &packages,
        &post_apply,
        show_values,
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

    #[test]
    fn status_role_accents_actionable_states() {
        assert_eq!(status_role("pending"), Some(Role::Accent));
        assert_eq!(status_role("out-of-date"), Some(Role::Accent));
        assert_eq!(status_role("installed"), None);
        assert_eq!(status_role("available"), None);
        assert_eq!(status_role("error"), None);
        assert_eq!(status_role(""), None);
    }
}

// --- Module CRUD helpers ---
