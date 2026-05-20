use super::*;
use cfgd_core::output::{Doc, Printer, Role, renderer::Table};

/// Build the `cfgd profile list` Doc from a populated entries vector + `--wide`
/// flag. Pure; the caller assembles the entries from disk.
pub fn build_profile_list_doc(entries: &[super::ProfileListEntry], wide: bool) -> Doc {
    let mut doc = Doc::new().heading("Available Profiles");

    if entries.is_empty() {
        doc = doc.status(Role::Info, "No profiles found");
        return doc.with_data(entries);
    }

    if wide {
        let mut t = Table::new(["Profile", "Active", "Inherits", "Modules"]);
        for e in entries {
            t = t.row([
                e.name.clone(),
                if e.active { "yes" } else { "-" }.to_string(),
                e.inherits.clone().unwrap_or_else(|| "-".into()),
                e.module_count.to_string(),
            ]);
        }
        doc = doc.table(t);
    } else {
        for entry in entries {
            if entry.active {
                doc = doc.status(Role::Ok, format!("{} (active)", entry.name));
            } else {
                doc = doc.status(Role::Info, entry.name.clone());
            }
        }
    }

    doc.with_data(entries)
}

/// Doc emitted when the profiles directory is absent.
pub fn build_profile_list_missing_doc(profiles_dir: &Path) -> Doc {
    let empty: Vec<super::ProfileListEntry> = Vec::new();
    Doc::new()
        .heading("Available Profiles")
        .status(
            Role::Warn,
            format!("Profiles directory not found: {}", profiles_dir.display()),
        )
        .with_data(&empty)
}

pub fn cmd_profile_list(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    let profiles_dir = profiles_dir(cli);

    if !profiles_dir.exists() {
        if printer.is_structured() {
            printer.emit(Doc::new().with_data(Vec::<super::ProfileListEntry>::new()));
            return Ok(());
        }
        printer.emit(build_profile_list_missing_doc(&profiles_dir));
        return Ok(());
    }

    let profiles = super::list_yaml_stems(&profiles_dir)?;

    let active = cli.profile.clone().unwrap_or_else(|| {
        config::load_config(&cli.config)
            .map(|c| c.spec.profile.unwrap_or_default())
            .unwrap_or_default()
    });

    let entries: Vec<super::ProfileListEntry> = profiles
        .iter()
        .map(|name| {
            let profile_path = profiles_dir.join(format!("{}.yaml", name));
            let (inherits, module_count) = if let Ok(doc) = config::load_profile(&profile_path) {
                let inh = if doc.spec.inherits.is_empty() {
                    None
                } else {
                    Some(doc.spec.inherits.join(", "))
                };
                (inh, doc.spec.modules.len())
            } else {
                // Try .yml extension
                let yml_path = profiles_dir.join(format!("{}.yml", name));
                if let Ok(doc) = config::load_profile(&yml_path) {
                    let inh = if doc.spec.inherits.is_empty() {
                        None
                    } else {
                        Some(doc.spec.inherits.join(", "))
                    };
                    (inh, doc.spec.modules.len())
                } else {
                    (None, 0)
                }
            };
            super::ProfileListEntry {
                name: name.clone(),
                active: *name == active,
                inherits,
                module_count,
            }
        })
        .collect();

    printer.emit(build_profile_list_doc(&entries, printer.is_wide()));

    Ok(())
}
