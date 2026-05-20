use super::*;

pub(crate) fn parse_manager_package(s: &str) -> anyhow::Result<(String, String)> {
    let (mgr, pkg) = s.split_once(':').ok_or_else(|| {
        anyhow::anyhow!(
            "Invalid package format '{}' — expected manager:package (e.g. brew:curl)",
            s
        )
    })?;
    if mgr.is_empty() || pkg.is_empty() {
        anyhow::bail!(
            "Invalid package format '{}' — manager and package name cannot be empty",
            s
        );
    }
    Ok((mgr.to_string(), pkg.to_string()))
}

pub(crate) fn parse_secret_spec(s: &str) -> anyhow::Result<config::SecretSpec> {
    // Split on last colon so provider URLs like op://vault/item:~/target work correctly
    let (source, target) = s.rsplit_once(':').ok_or_else(|| {
        anyhow::anyhow!(
            "Invalid secret format '{}' — expected source:target (e.g. secrets/api-key.enc:~/.config/app/key)",
            s
        )
    })?;
    if source.is_empty() || target.is_empty() {
        anyhow::bail!(
            "Invalid secret format '{}' — source and target cannot be empty",
            s
        );
    }
    Ok(config::SecretSpec {
        source: source.to_string(),
        target: Some(PathBuf::from(target)),
        template: None,
        backend: None,
        envs: None,
    })
}

pub(crate) fn update_script_list(
    scripts_opt: &mut Option<config::ScriptSpec>,
    add: &[String],
    remove: &[String],
    label: &str,
    field: fn(&mut config::ScriptSpec) -> &mut Vec<config::ScriptEntry>,
    printer: &cfgd_core::output::Printer,
) -> u32 {
    use cfgd_core::output::Role;
    let mut changes = 0u32;
    for script in add {
        let scripts = scripts_opt.get_or_insert_with(Default::default);
        let list = field(scripts);
        let entry = config::ScriptEntry::Simple(script.clone());
        if list.contains(&entry) {
            printer.status_simple(
                Role::Warn,
                format!("{} script '{}' already exists", label, script),
            );
            continue;
        }
        list.push(entry);
        printer.status_simple(Role::Ok, format!("Added {}: {}", label, script));
        changes += 1;
    }
    for script in remove {
        if let Some(scripts) = scripts_opt.as_mut() {
            let list = field(scripts);
            let before = list.len();
            list.retain(|e| e.run_str() != script.as_str());
            if list.len() < before {
                printer.status_simple(Role::Ok, format!("Removed {}: {}", label, script));
                changes += 1;
            } else {
                printer.status_simple(
                    Role::Warn,
                    format!("{} script '{}' not found", label, script),
                );
            }
        } else {
            printer.status_simple(
                Role::Warn,
                format!("{} script '{}' not found", label, script),
            );
        }
    }
    changes
}
