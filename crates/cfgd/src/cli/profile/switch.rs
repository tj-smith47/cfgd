use super::*;
use cfgd_core::output_v2::{Doc, Printer as PrinterV2, Role};

pub fn cmd_profile_switch(cli: &Cli, name: &str, v2_printer: &PrinterV2) -> anyhow::Result<()> {
    v2_printer.heading("Switch Profile");

    let config_dir = super::config_dir(cli);
    let config_path = config_dir.join("cfgd.yaml");
    if !config_path.exists() {
        v2_printer.emit(super::build_profile_error_doc(
            name,
            "no_config",
            MSG_NO_CONFIG,
            serde_json::json!({ "configPath": config_path.display().to_string() }),
        ));
        anyhow::bail!("{}", MSG_NO_CONFIG);
    }

    // Verify the target profile exists
    let profiles_dir = config_dir.join("profiles");
    let profile_path = profiles_dir.join(format!("{}.yaml", name));
    if !profile_path.exists() {
        // List available profiles for the error message
        let available = super::list_yaml_stems(&profiles_dir).unwrap_or_default();
        let hint = if available.is_empty() {
            String::new()
        } else {
            format!("\nAvailable profiles: {}", available.join(", "))
        };
        v2_printer.emit(super::build_profile_error_doc(
            name,
            "not_found",
            format!(
                "Profile '{}' not found at {}{}",
                name,
                profile_path.display(),
                hint
            ),
            serde_json::json!({
                "profilePath": profile_path.display().to_string(),
                "available": available,
            }),
        ));
        anyhow::bail!(
            "Profile '{}' not found at {}{}",
            name,
            profile_path.display(),
            hint
        );
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
    v2_printer.emit(doc);

    Ok(())
}
