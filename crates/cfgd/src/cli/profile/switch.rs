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

    // Verify the target profile exists
    let profiles_dir = config_dir.join("profiles");
    let profile_path = profiles_dir.join(format!("{}.yaml", name));
    if !profile_path.exists() {
        let available = super::list_yaml_stems(&profiles_dir).unwrap_or_default();
        let mut hints = Vec::new();
        if !available.is_empty() {
            hints.push(format!("Available profiles: {}", available.join(", ")));
        }
        // Carry the typed `ConfigError::ProfileNotFound` in the chain so the
        // exit-code downcast in `main.rs` resolves to ExitCode::NotFound (6);
        // the attached CliErrorMeta still drives the rich `not_found` payload.
        return Err(crate::cli::cli_error_ctx_with_hints(
            cfgd_core::errors::CfgdError::Config(cfgd_core::errors::ConfigError::ProfileNotFound {
                name: name.to_string(),
            })
            .into(),
            name,
            "not_found",
            format!("Profile '{}' not found at {}", name, profile_path.posix()),
            serde_json::json!({
                "profilePath": profile_path.display().to_string(),
                "available": available,
            }),
            hints,
        ));
    }

    // Read current config, update profile field, write back
    let contents = std::fs::read_to_string(&config_path)?;
    let mut cfg: config::CfgdConfig = config::parse_config(&contents, &config_path)?;
    let old_profile = cfg.spec.profile.clone().unwrap_or_default();
    cfg.spec.profile = Some(name.to_string());

    let yaml = serde_yaml::to_string(&cfg)?;
    cfgd_core::atomic_write_str(&config_path, &yaml)?;

    let doc = Doc::new()
        .status(
            Role::Ok,
            format!("Switched profile: {} → {}", old_profile, name),
        )
        .hint(MSG_RUN_APPLY)
        .with_data(serde_json::json!({
            "from": old_profile,
            "to": name,
        }));
    printer.emit(doc);

    Ok(())
}
