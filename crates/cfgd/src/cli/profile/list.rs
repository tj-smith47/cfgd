use super::*;
use cfgd_core::PathDisplayExt;
use cfgd_core::output::{Doc, Printer, Role, renderer::Table};

/// Build the `cfgd profile list` Doc from a populated entries vector + `--wide`
/// flag. Pure; the caller assembles the entries from disk.
pub fn build_profile_list_doc(entries: &[super::ProfileListEntry], wide: bool) -> Doc {
    let mut doc = Doc::new().heading("Available Profiles");

    if entries.is_empty() {
        doc = doc.status(Role::Info, "No profiles found");
        return doc.with_data(entries);
    }

    let table = if wide {
        let mut t = Table::new(["Profile", "Active", "Inherits", "Modules"]);
        for e in entries {
            t = t.row([
                e.name.clone(),
                cfgd_core::yes_no(Some(e.active)).to_string(),
                e.inherits
                    .clone()
                    .unwrap_or_else(|| cfgd_core::ABSENT.into()),
                e.module_count.to_string(),
            ]);
        }
        t
    } else {
        // The narrow table carries every fact the wide one does, folded into
        // one cell: a profile's parents and its module count both answer "what
        // does this profile bring", and neither is worth its own column at 80.
        let mut t = Table::new(["Profile", "Active", "Contents"]);
        for e in entries {
            let mut contents = cfgd_core::pluralize(e.module_count, "module");
            if let Some(inherits) = e.inherits.as_deref() {
                contents.push_str(", inherits ");
                contents.push_str(inherits);
            }
            t = t.row([
                e.name.clone(),
                cfgd_core::yes_no(Some(e.active)).to_string(),
                contents,
            ]);
        }
        t
    };

    doc.table(table.without_unfillable_columns())
        .with_data(entries)
}

/// Doc emitted when the profiles directory is absent.
pub fn build_profile_list_missing_doc(profiles_dir: &Path) -> Doc {
    let empty: Vec<super::ProfileListEntry> = Vec::new();
    Doc::new()
        .heading("Available Profiles")
        .status(
            Role::Warn,
            format!(
                "Profiles directory not found: {}",
                cfgd_core::fold_home_in_text(&profiles_dir.display_posix())
            ),
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

    let profiles = cfgd_core::config::scan_profiles(&profiles_dir)
        .map_err(cfgd_core::errors::CfgdError::Config)?;

    let active = match &cli.profile {
        Some(p) => p.clone(),
        None => match config::load_config(&cli.config) {
            Ok(mut c) => {
                drain_config_deprecations(printer, &mut c);
                c.spec.profile.unwrap_or_default()
            }
            Err(_) => String::new(),
        },
    };

    let entries: Vec<super::ProfileListEntry> = profiles
        .iter()
        .map(|entry| {
            let (inherits, module_count) = if let Ok(doc) = config::load_profile(&entry.path) {
                let inh = if doc.spec.inherits.is_empty() {
                    None
                } else {
                    Some(doc.spec.inherits.join(", "))
                };
                (inh, doc.spec.modules.len())
            } else {
                (None, 0)
            };
            super::ProfileListEntry {
                name: entry.name.clone(),
                active: entry.name == active,
                inherits,
                module_count,
            }
        })
        .collect();

    printer.emit(build_profile_list_doc(&entries, printer.is_wide()));

    Ok(())
}
