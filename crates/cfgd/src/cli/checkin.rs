use super::*;

pub(super) fn cmd_checkin(
    cli: &Cli,
    printer: &Printer,
    server_url: &str,
    api_key: Option<&str>,
    device_id: Option<&str>,
) -> anyhow::Result<()> {
    let (cfg, resolved) = load_config_and_profile(cli, printer)?;
    let registry = build_registry_with_profile(&resolved.merged.packages);

    // Try stored device credential first, fall back to explicit args
    let stored_cred = cfgd_core::server_client::load_credential().ok().flatten();
    let client = if api_key.is_none() {
        if let Some(ref cred) = stored_cred {
            if cred.server_url.trim_end_matches('/') == server_url.trim_end_matches('/') {
                cfgd_core::server_client::ServerClient::from_credential(cred)
            } else {
                let did = device_id
                    .map(|s| s.to_string())
                    .unwrap_or_else(default_device_id);
                cfgd_core::server_client::ServerClient::new(server_url, None, &did)
            }
        } else {
            let did = device_id
                .map(|s| s.to_string())
                .unwrap_or_else(default_device_id);
            cfgd_core::server_client::ServerClient::new(server_url, None, &did)
        }
    } else {
        let did = device_id
            .map(|s| s.to_string())
            .unwrap_or_else(default_device_id);
        cfgd_core::server_client::ServerClient::new(server_url, api_key, &did)
    };

    // Compute config hash
    let config_yaml = serde_yaml::to_string(&resolved.merged.system)
        .map_err(|e| anyhow::anyhow!("failed to serialize system config: {}", e))?;
    let config_hash = cfgd_core::sha256_hex(config_yaml.as_bytes());

    // Collect compliance summary if enabled
    let compliance_summary = if let Some(ref compliance_cfg) = cfg.spec.compliance {
        if compliance_cfg.enabled {
            let profile_name = cfg.active_profile().unwrap_or("unknown");
            match cfgd_core::compliance::collect_snapshot(
                profile_name,
                &resolved.merged,
                &registry,
                &compliance_cfg.scope,
                &[],
            ) {
                Ok(snapshot) => {
                    printer.info(&format!(
                        "Compliance: {} compliant, {} warning, {} violation",
                        snapshot.summary.compliant,
                        snapshot.summary.warning,
                        snapshot.summary.violation,
                    ));
                    Some(snapshot.summary)
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to collect compliance snapshot for checkin");
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    // Check in
    let resp = client
        .checkin(&config_hash, compliance_summary, printer)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    printer.key_value("Server status", &resp.status);
    printer.key_value("Config changed", &resp.config_changed.to_string());

    // Save desired config from server for next reconcile
    if let Some(ref desired) = resp.desired_config {
        match cfgd_core::state::save_pending_server_config(desired) {
            Ok(path) => {
                printer.warning(&format!(
                    "Server pushed a new desired config — saved to {}",
                    path.display()
                ));
                printer.info(MSG_RUN_APPLY);
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to save pending server config");
                printer.warning("Server sent desired config but failed to save it locally");
            }
        }
    }

    // Collect and report drift
    let mut all_drifts = Vec::new();
    let available = registry.available_system_configurators();

    for configurator in &available {
        let key = configurator.name();
        let desired = match resolved.merged.system.get(key) {
            Some(v) => v,
            None => continue,
        };
        if let Ok(drifts) = configurator.diff(desired) {
            all_drifts.extend(drifts);
        }
    }

    if !all_drifts.is_empty() {
        client
            .report_drift(&all_drifts, printer)
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        printer.warning(&format!("{} drift items reported", all_drifts.len()));
    } else {
        printer.success("No drift to report");
    }

    Ok(())
}
