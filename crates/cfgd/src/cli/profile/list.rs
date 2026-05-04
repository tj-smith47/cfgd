use super::*;

pub(crate) fn cmd_profile_list(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    let profiles_dir = profiles_dir(cli);

    if !profiles_dir.exists() {
        if printer.is_structured() {
            printer.write_structured(&Vec::<super::ProfileListEntry>::new());
            return Ok(());
        }
        printer.header("Available Profiles");
        printer.warning(&format!(
            "Profiles directory not found: {}",
            profiles_dir.display()
        ));
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

    if printer.write_structured(&entries) {
        return Ok(());
    }

    printer.header("Available Profiles");

    if printer.is_wide() {
        let rows: Vec<Vec<String>> = entries
            .iter()
            .map(|e| {
                vec![
                    e.name.clone(),
                    if e.active { "yes" } else { "-" }.to_string(),
                    e.inherits.clone().unwrap_or_else(|| "-".into()),
                    e.module_count.to_string(),
                ]
            })
            .collect();
        printer.table(&["Profile", "Active", "Inherits", "Modules"], &rows);
    } else {
        for entry in &entries {
            if entry.active {
                printer.success(&format!("{} (active)", entry.name));
            } else {
                printer.info(&entry.name);
            }
        }
    }

    if entries.is_empty() {
        printer.info("No profiles found");
    }

    Ok(())
}
