use super::*;

pub(crate) fn cmd_profile_switch(cli: &Cli, name: &str, printer: &Printer) -> anyhow::Result<()> {
    printer.header("Switch Profile");

    let config_dir = super::config_dir(cli);
    let config_path = config_dir.join("cfgd.yaml");
    if !config_path.exists() {
        anyhow::bail!("{}", MSG_NO_CONFIG);
    }

    // Verify the target profile exists
    let profiles_dir = config_dir.join("profiles");
    let profile_path = profiles_dir.join(format!("{}.yaml", name));
    if !profile_path.exists() {
        // List available profiles for the error message
        let mut hint = String::new();
        let available = super::list_yaml_stems(&profiles_dir).unwrap_or_default();
        if !available.is_empty() {
            hint = format!("\nAvailable profiles: {}", available.join(", "));
        }
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

    printer.success(&format!("Switched profile: {} → {}", old_profile, name));
    printer.info(MSG_RUN_APPLY);

    Ok(())
}
