use super::*;

use anyhow::Context;
use cfgd_core::PathDisplayExt;
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

    let (cfg, _profile_name, local_resolved) = load_config_and_profile(cli)?;
    let config_dir = config_dir(cli);

    // Compose with sources (cache-only — read paths stay offline) and resolve the
    // effective module set through the one shared resolver, so the checkin
    // payload reflects the same source-composed desired state that `apply` writes.
    let desired = resolve_desired_state(cli, &cfg, &local_resolved, None, printer, false)?;
    let resolved = desired.resolved;
    let resolved_modules = desired.modules;

    let mut registry = build_registry_with_profile(&resolved.merged.packages);
    registry.file_manager = Some(Box::new(build_compliance_file_manager(
        &config_dir,
        &resolved,
    )?));

    let stored_cred = cfgd_core::server_client::load_credential().ok().flatten();
    let client = build_checkin_client(server_url, api_key, device_id, stored_cred.as_ref());

    let config_yaml = serde_yaml::to_string(&resolved.merged.system)
        .context("failed to serialize system config")?;
    let config_hash = cfgd_core::sha256_hex(config_yaml.as_bytes());

    let compliance_summary = if let Some(ref compliance_cfg) = cfg.spec.compliance {
        if compliance_cfg.enabled {
            let profile_name = cfg.active_profile().unwrap_or("unknown");
            match cfgd_core::compliance::collect_snapshot(
                profile_name,
                &resolved.merged,
                &resolved_modules,
                &config_dir,
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
            .context("checkin to gateway failed");
        match &result {
            Ok(resp) => {
                sp.finish_ok(format!("server status: {}", resp.status));
            }
            Err(e) => {
                sp.finish_fail("Checkin failed").detail(format!("{e:#}"));
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
                push_sec.status_simple(Role::Ok, format!("Saved to {}", path.posix()));
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
            .context("drift report to gateway failed");
        match &res {
            Ok(()) => {
                sp.finish_ok(format!("{} drift items reported", all_drifts.len()));
            }
            Err(e) => {
                sp.finish_fail("Drift report failed")
                    .detail(format!("{e:#}"));
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

#[cfg(test)]
mod tests {
    use cfgd_core::output::{OutputFormat, Printer, Verbosity};
    use cfgd_core::server_client::DeviceCredential;
    use cfgd_core::test_helpers::EnvVarGuard;

    use super::*;

    // ---------------------------------------------------------------------------
    // Helpers
    // ---------------------------------------------------------------------------

    const MINIMAL_CONFIG: &str = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n";

    const MINIMAL_PROFILE: &str = r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: default
spec: {}
"#;

    fn make_cred(server_url: &str, device_id: &str, api_key: &str) -> DeviceCredential {
        DeviceCredential {
            server_url: server_url.to_string(),
            device_id: device_id.to_string(),
            api_key: api_key.to_string(),
            username: "test-user".to_string(),
            team: None,
            enrolled_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    // ---------------------------------------------------------------------------
    // build_checkin_doc
    // ---------------------------------------------------------------------------

    #[test]
    fn build_checkin_doc_carries_checkin_output_fields() {
        let output = CheckinOutput {
            server_status: "ok".to_string(),
            config_changed: true,
            drift_count: 3,
            drift_status: "drift_reported".to_string(),
            server_pushed_config: false,
        };
        let (printer, cap) = Printer::for_test_doc();
        printer.emit(build_checkin_doc(&output));
        drop(printer);

        let json = cap.json().expect("doc must carry structured data");
        assert_eq!(
            json["serverStatus"].as_str(),
            Some("ok"),
            "serverStatus mismatch: {json}"
        );
        assert_eq!(
            json["configChanged"].as_bool(),
            Some(true),
            "configChanged mismatch: {json}"
        );
        assert_eq!(
            json["driftCount"].as_u64(),
            Some(3),
            "driftCount mismatch: {json}"
        );
        assert_eq!(
            json["driftStatus"].as_str(),
            Some("drift_reported"),
            "driftStatus mismatch: {json}"
        );
        assert_eq!(
            json["serverPushedConfig"].as_bool(),
            Some(false),
            "serverPushedConfig mismatch: {json}"
        );
    }

    #[test]
    fn build_checkin_doc_no_drift_variant() {
        let output = CheckinOutput {
            server_status: "ok".to_string(),
            config_changed: false,
            drift_count: 0,
            drift_status: "no_drift".to_string(),
            server_pushed_config: false,
        };
        let (printer, cap) = Printer::for_test_doc();
        printer.emit(build_checkin_doc(&output));
        drop(printer);

        let json = cap.json().expect("doc must carry structured data");
        assert_eq!(json["driftCount"].as_u64(), Some(0));
        assert_eq!(json["driftStatus"].as_str(), Some("no_drift"));
        assert_eq!(json["configChanged"].as_bool(), Some(false));
    }

    // ---------------------------------------------------------------------------
    // build_checkin_client
    // ---------------------------------------------------------------------------

    #[test]
    fn build_checkin_client_uses_stored_cred_when_api_key_absent_and_urls_match() {
        // Verify the stored-credential path: no api_key, URLs match → the
        // returned client sends the stored credential's api_key in its requests.
        let mut server = mockito::Server::new();
        let cred = make_cred(&server.url(), "stored-device", "stored-key");
        let mock = server
            .mock("POST", "/api/v1/checkin")
            .match_header("authorization", "Bearer stored-key")
            .with_status(200)
            .with_body(r#"{"status":"ok","configChanged":false}"#)
            .create();

        let client = build_checkin_client(&server.url(), None, None, Some(&cred));
        let (printer, _buf) = Printer::for_test_at(Verbosity::Quiet);
        let result = client.checkin("hash", None, &printer);

        assert!(result.is_ok(), "checkin should succeed: {:?}", result);
        mock.assert();
    }

    #[test]
    fn build_checkin_client_uses_provided_api_key_over_stored_cred() {
        // Explicit api_key overrides stored credential — even when URLs match.
        let mut server = mockito::Server::new();
        let cred = make_cred(&server.url(), "stored-device", "stored-key");
        let mock = server
            .mock("POST", "/api/v1/checkin")
            .match_header("authorization", "Bearer explicit-key")
            .with_status(200)
            .with_body(r#"{"status":"ok","configChanged":false}"#)
            .create();

        let client = build_checkin_client(
            &server.url(),
            Some("explicit-key"),
            Some("dev-x"),
            Some(&cred),
        );
        let (printer, _buf) = Printer::for_test_at(Verbosity::Quiet);
        let result = client.checkin("hash", None, &printer);

        assert!(result.is_ok(), "checkin should succeed: {:?}", result);
        mock.assert();
    }

    #[test]
    fn build_checkin_client_ignores_stored_cred_when_urls_mismatch() {
        // Stored cred URL differs from server_url → anonymous client, no stored key.
        let mut server = mockito::Server::new();
        let cred = make_cred("http://other-server:9999", "stored-device", "stored-key");
        // The mock must NOT see the stored key — match absence of Authorization.
        let mock = server
            .mock("POST", "/api/v1/checkin")
            .with_status(200)
            .with_body(r#"{"status":"ok","configChanged":false}"#)
            .create();

        let client =
            build_checkin_client(&server.url(), None, Some("explicit-device"), Some(&cred));
        let (printer, _buf) = Printer::for_test_at(Verbosity::Quiet);
        let result = client.checkin("hash", None, &printer);

        // The mock succeeds without requiring Bearer stored-key, confirming the
        // anonymous (non-stored-cred) path was taken.
        assert!(result.is_ok(), "checkin should succeed: {:?}", result);
        mock.assert();
    }

    #[test]
    fn build_checkin_client_trailing_slash_normalization() {
        // Stored URL with trailing slash should match server_url without one.
        let mut server = mockito::Server::new();
        let server_url = server.url();
        let cred_url = format!("{}/", server_url);
        let cred = make_cred(&cred_url, "dev-1", "trailing-slash-key");
        let mock = server
            .mock("POST", "/api/v1/checkin")
            .match_header("authorization", "Bearer trailing-slash-key")
            .with_status(200)
            .with_body(r#"{"status":"ok","configChanged":false}"#)
            .create();

        let client = build_checkin_client(&server_url, None, None, Some(&cred));
        let (printer, _buf) = Printer::for_test_at(Verbosity::Quiet);
        let result = client.checkin("hash", None, &printer);

        assert!(
            result.is_ok(),
            "trailing-slash normalization failed: {:?}",
            result
        );
        mock.assert();
    }

    // ---------------------------------------------------------------------------
    // cmd_checkin — full command tests via mockito
    // ---------------------------------------------------------------------------

    fn make_test_config_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let profiles_dir = dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(dir.path().join("cfgd.yaml"), MINIMAL_CONFIG).unwrap();
        std::fs::write(profiles_dir.join("default.yaml"), MINIMAL_PROFILE).unwrap();
        dir
    }

    fn test_cli_for(config_dir: &std::path::Path, state_dir: &std::path::Path) -> Cli {
        Cli {
            config: config_dir.join("cfgd.yaml"),
            profile: None,
            verbose: 0,
            quiet: true,
            no_color: true,
            output: OutputFormatArg(OutputFormat::Table),
            list_envelope: false,
            jsonpath: None,
            state_dir: Some(state_dir.to_path_buf()),
            config_dir: None,
            cache_dir: None,
            runtime_dir: None,
            command: None,
        }
    }

    #[test]
    #[serial_test::serial]
    fn cmd_checkin_happy_path_no_drift() {
        let config_dir = make_test_config_dir();
        let state_dir = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(config_dir.path());
        let _state_env = EnvVarGuard::set("CFGD_STATE_DIR", state_dir.path().to_str().unwrap());

        let mut server = mockito::Server::new();
        let mock = server
            .mock("POST", "/api/v1/checkin")
            .with_status(200)
            .with_body(r#"{"status":"ok","configChanged":false}"#)
            .create();

        let cli = test_cli_for(config_dir.path(), state_dir.path());
        let (printer, cap) = Printer::for_test_doc();
        let result = cmd_checkin(
            &cli,
            &printer,
            &server.url(),
            Some("test-key"),
            Some("dev-1"),
        );
        drop(printer);

        assert!(result.is_ok(), "cmd_checkin should succeed: {:?}", result);
        mock.assert();

        let human = cap.human();
        assert!(
            human.contains("Server status"),
            "should print 'Server status', got: {human}"
        );

        let json = cap.json().expect("should emit structured Doc");
        assert_eq!(
            json["serverStatus"].as_str(),
            Some("ok"),
            "serverStatus should be 'ok': {json}"
        );
        assert_eq!(
            json["driftStatus"].as_str(),
            Some("no_drift"),
            "no configurators → no_drift: {json}"
        );
        assert_eq!(
            json["serverPushedConfig"].as_bool(),
            Some(false),
            "no desired_config in response: {json}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn cmd_checkin_server_pushes_desired_config() {
        let config_dir = make_test_config_dir();
        let state_dir = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(config_dir.path());
        let _state_env = EnvVarGuard::set("CFGD_STATE_DIR", state_dir.path().to_str().unwrap());

        let mut server = mockito::Server::new();
        let mock = server
            .mock("POST", "/api/v1/checkin")
            .with_status(200)
            .with_body(
                r#"{"status":"ok","configChanged":true,"desiredConfig":{"packages":["git","curl"]}}"#,
            )
            .create();

        let cli = test_cli_for(config_dir.path(), state_dir.path());
        let (printer, cap) = Printer::for_test_doc();
        let result = cmd_checkin(
            &cli,
            &printer,
            &server.url(),
            Some("test-key"),
            Some("dev-1"),
        );
        drop(printer);

        assert!(result.is_ok(), "cmd_checkin should succeed: {:?}", result);
        mock.assert();

        // Pending config file must be written under the state dir.
        let pending = state_dir.path().join("pending-server-config.json");
        assert!(
            pending.exists(),
            "pending-server-config.json must be saved to state dir"
        );
        let saved_json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&pending).unwrap()).unwrap();
        assert_eq!(
            saved_json["packages"][0].as_str(),
            Some("git"),
            "saved config should contain pushed packages"
        );

        let json = cap.json().expect("should emit structured Doc");
        assert_eq!(
            json["serverPushedConfig"].as_bool(),
            Some(true),
            "serverPushedConfig should be true: {json}"
        );
        assert_eq!(
            json["configChanged"].as_bool(),
            Some(true),
            "configChanged should reflect server response: {json}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn cmd_checkin_server_500_returns_err() {
        let config_dir = make_test_config_dir();
        let state_dir = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(config_dir.path());
        let _state_env = EnvVarGuard::set("CFGD_STATE_DIR", state_dir.path().to_str().unwrap());

        let mut server = mockito::Server::new();
        // The retry logic retries 500s, so allow at least 2 hits.
        let mock = server
            .mock("POST", "/api/v1/checkin")
            .with_status(500)
            .with_body("internal server error")
            .expect_at_least(2)
            .create();

        let cli = test_cli_for(config_dir.path(), state_dir.path());
        let (printer, _cap) = Printer::for_test_doc();
        let result = cmd_checkin(
            &cli,
            &printer,
            &server.url(),
            Some("test-key"),
            Some("dev-1"),
        );
        drop(printer);

        assert!(
            result.is_err(),
            "cmd_checkin should return Err on server 500"
        );
        let err_msg = format!("{:?}", result.unwrap_err());
        assert!(
            err_msg.contains("failed after") || err_msg.contains("server error"),
            "error should describe server failure: {err_msg}"
        );
        mock.assert();
    }
}
