use super::*;
use cfgd_core::PathDisplayExt;
use cfgd_core::output::{Doc, Printer, Role};

pub fn cmd_profile_switch(cli: &Cli, name: &str, printer: &Printer) -> anyhow::Result<()> {
    printer.heading("Switch Profile");

    let config_dir = super::config_dir(cli);
    let config_path = config_dir.join("cfgd.yaml");
    if !config_path.exists() {
        return Err(no_config_error(printer, &config_path));
    }

    // Verify the target profile exists (either layout form)
    let profiles_dir = config_dir.join("profiles");
    match cfgd_core::config::find_profile_path(&profiles_dir, name) {
        Ok(_) => {}
        Err(e @ cfgd_core::errors::ConfigError::ProfileNotFound { .. }) => {
            let available = super::available_profile_names(&profiles_dir);
            let mut hints = Vec::new();
            if !available.is_empty() {
                hints.push(cfgd_core::output::HintCommands::from(format!(
                    "Available profiles: {}",
                    available.join(", ")
                )));
            }
            // Carry the typed `ConfigError::ProfileNotFound` in the chain so the
            // exit-code downcast in `main.rs` resolves to ExitCode::NotFound (6);
            // the attached CliErrorMeta still drives the rich `not_found` payload.
            return Err(crate::cli::cli_error_ctx_with_hints(
                cfgd_core::errors::CfgdError::Config(e).into(),
                name,
                "not_found",
                format!("Profile '{}' not found in {}", name, profiles_dir.posix()),
                serde_json::json!({
                    "profilesDir": cfgd_core::to_posix_string(&profiles_dir),
                    "available": available,
                }),
                hints,
            ));
        }
        Err(e) => return Err(cfgd_core::errors::CfgdError::Config(e).into()),
    }

    let mut old_profile = String::new();
    let mut cfg = crate::cli::mutate_config_yaml(&config_path, |raw| {
        let spec = crate::cli::config_cmd::spec_mapping_mut(raw, &config_path)?;
        let previous = spec.insert(
            serde_yaml::Value::String("profile".into()),
            serde_yaml::Value::String(name.to_string()),
        );
        if let Some(previous) = previous.as_ref().and_then(serde_yaml::Value::as_str) {
            old_profile = previous.to_string();
        }
        Ok(())
    })?;
    drain_config_deprecations(printer, &mut cfg);

    let doc = Doc::new()
        .status(
            Role::Ok,
            format!(
                "Switched profile: {} {} {}",
                old_profile,
                printer.arrow(),
                name
            ),
        )
        .hint(crate::cli::success_next_step(
            crate::cli::Mutation::ProfileSwitched,
        ))
        .with_data(serde_json::json!({
            "from": old_profile,
            "to": name,
        }));
    printer.emit(doc);

    Ok(())
}
