use super::*;

use cfgd_core::output::{Doc, Printer, Role};
use cfgd_core::server_client::{DeviceCredential, ServerClient};

pub fn cmd_checkin(
    cli: &Cli,
    printer: &Printer,
    server_url: &str,
    api_key: Option<&str>,
    device_id: Option<&str>,
) -> anyhow::Result<()> {
    printer.heading("Checkin");

    let (cfg, _profile_name, resolved) = load_config_and_profile(cli)?;
    let registry = build_registry_with_profile(&resolved.merged.packages);

    let stored_cred = cfgd_core::server_client::load_credential().ok().flatten();
    let client = build_checkin_client(server_url, api_key, device_id, stored_cred.as_ref());

    let config_yaml = serde_yaml::to_string(&resolved.merged.system)
        .map_err(|e| anyhow::anyhow!("failed to serialize system config: {}", e))?;
    let config_hash = cfgd_core::sha256_hex(config_yaml.as_bytes());

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
                    printer.kv(
                        "Compliance",
                        format!(
                            "{} compliant, {} warning, {} violation",
                            snapshot.summary.compliant,
                            snapshot.summary.warning,
                            snapshot.summary.violation,
                        ),
                    );
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

    let resp = {
        let sp = printer.spinner("Posting to gateway");
        let result = client
            .checkin(&config_hash, compliance_summary, printer)
            .map_err(|e| anyhow::anyhow!("{}", e));
        match &result {
            Ok(resp) => {
                sp.finish_ok(format!("server status: {}", resp.status));
            }
            Err(e) => {
                sp.finish_fail("Checkin failed").detail(e.to_string());
            }
        }
        result?
    };

    printer.kv("Server status", &resp.status);
    printer.kv("Config changed", resp.config_changed.to_string());

    if let Some(ref desired) = resp.desired_config {
        printer.status_simple(Role::Warn, "Server pushed desired config");
        let push_sec = printer.section("Server config");
        match cfgd_core::state::save_pending_server_config(desired) {
            Ok(path) => {
                push_sec.status_simple(Role::Ok, format!("Saved to {}", path.display()));
                push_sec.status_simple(Role::Info, MSG_RUN_APPLY);
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to save pending server config");
                push_sec.status_simple(
                    Role::Warn,
                    "Server sent desired config but failed to save it locally",
                );
            }
        }
    }

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

    let drift_status = if !all_drifts.is_empty() {
        let sp = printer.spinner("Reporting drift");
        let res = client
            .report_drift(&all_drifts, printer)
            .map_err(|e| anyhow::anyhow!("{}", e));
        match &res {
            Ok(()) => {
                sp.finish_ok(format!("{} drift items reported", all_drifts.len()));
            }
            Err(e) => {
                sp.finish_fail("Drift report failed").detail(e.to_string());
            }
        }
        res?;
        "drift_reported"
    } else {
        printer.status_simple(Role::Info, "No drift to report");
        "no_drift"
    };

    printer.emit(build_checkin_doc(&CheckinOutput {
        server_status: resp.status.clone(),
        config_changed: resp.config_changed,
        drift_count: all_drifts.len(),
        drift_status: drift_status.to_string(),
        server_pushed_config: resp.desired_config.is_some(),
    }));

    Ok(())
}

/// Construct the `ServerClient` for the checkin request, preferring a stored
/// device credential whose `server_url` matches `server_url` (when no explicit
/// `api_key` is provided) over a fresh anonymous client.
fn build_checkin_client(
    server_url: &str,
    api_key: Option<&str>,
    device_id: Option<&str>,
    stored_cred: Option<&DeviceCredential>,
) -> ServerClient {
    if api_key.is_none()
        && let Some(cred) = stored_cred
        && cred.server_url.trim_end_matches('/') == server_url.trim_end_matches('/')
    {
        return ServerClient::from_credential(cred);
    }
    let did = device_id
        .map(|s| s.to_string())
        .unwrap_or_else(default_device_id);
    ServerClient::new(server_url, api_key, &did)
}

/// Sole place the Checkin buffered Doc is built. Keeps real `cmd_checkin` and
/// snapshot tests sharing one Doc-construction seam.
pub fn build_checkin_doc(output: &CheckinOutput) -> Doc {
    Doc::new().with_data(output)
}
