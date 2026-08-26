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

    let ctx = RunContext::new(cli, printer);
    let (cfg, _profile_name, local_resolved) = ctx.config_and_profile()?;
    let config_dir = ctx.config_dir();

    // Compose with sources (cache-only — read paths stay offline) and resolve the
    // effective module set through the one shared resolver, so the checkin
    // payload reflects the same source-composed desired state that `apply` writes.
    let mut desired = resolve_desired_state(
        &ctx,
        cfg,
        local_resolved,
        &[],
        false,
        printer,
        false,
        composition::ConstraintMode::Report,
    )?;
    // Taken before the other fields, because a partial move out of `desired`
    // would block the `&mut self` this accessor needs.
    let mut registry = desired.take_registry(cfg);
    let resolved = desired.resolved;
    let resolved_modules = desired.modules;

    registry.file_manager = Some(Box::new(build_compliance_file_manager(
        config_dir,
        &resolved,
        Some(&ctx),
    )?));

    let stored_cred = cfgd_core::server_client::load_credential().ok().flatten();
    let client = build_checkin_client(server_url, api_key, device_id, stored_cred.as_ref());

    // The effective (profile ⊕ modules) system map, not the profile's own: a
    // module's system settings are desired state like any other, and reading the
    // profile-only view hid them from BOTH surfaces below — the hash the gateway
    // uses to tell one desired config from another never moved when a module's
    // settings changed, and the drift scan never checked a setting only a module
    // declared.
    let system = cfgd_core::effective::effective_system_map(&resolved.merged, &resolved_modules);
    let config_yaml =
        serde_yaml::to_string(&system).context("failed to serialize system config")?;
    let config_hash = cfgd_core::sha256_hex(config_yaml.as_bytes());

    // The machine is diffed ONCE per checkin, and not until someone asks. Both
    // consumers read from this cell: the compliance snapshot's system checks
    // below, and the drift report sent after the gateway answers. Sharing it
    // also fixes the order the two used to disagree on — every drift is now in
    // effective-system-map order, the same order compliance records it in.
    // Lazily, because the diff shells out to every configurator the profile
    // declares: a checkin whose gateway call fails must not have paid for a
    // scan of the machine nobody will read.
    let system_diffs: std::cell::OnceCell<Vec<cfgd_core::compliance::SystemDiff>> =
        std::cell::OnceCell::new();
    let diff_system = || {
        cfgd_core::compliance::collect_system_diffs(&resolved.merged, &resolved_modules, &registry)
    };

    let compliance_summary = if let Some(ref compliance_cfg) = cfg.spec.compliance {
        if compliance_cfg.enabled {
            let profile_name = cfg.active_profile().unwrap_or("unknown");
            let checkin_state = ctx.state()?;
            match cfgd_core::compliance::collect_snapshot(
                profile_name,
                &resolved.merged,
                &resolved_modules,
                config_dir,
                &registry,
                &compliance_cfg.scope,
                &[],
                printer,
                checkin_state,
                Some(system_diffs.get_or_init(diff_system)),
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
        // `client.checkin` narrates through the bare `&Printer` it's handed
        // (`status_simple("Checking in with device gateway")`) rather than a
        // bound `SectionGuard`, so it needs a real section to inherit depth
        // from — without one that line renders at depth 0 whatever else this
        // command has already printed.
        //
        // No bar of its own: the round-trip is narrated one layer down under
        // the same label, and two spinners animating for one request read as
        // two requests. What this section owns is the VERDICT.
        let gateway_sec = printer.section("Gateway");
        let _inherit = printer.depth_inheritance();
        let result = client
            .checkin(&config_hash, compliance_summary, printer)
            .context("checkin to gateway failed");
        match &result {
            Ok(_) => {
                // The VERDICT of the round-trip, not the server's own status
                // string: that is a fact about the response and is stated
                // once, by the `Server status` kv row below.
                gateway_sec.status_simple(Role::Ok, "Checked in");
            }
            Err(e) => {
                gateway_sec
                    .status(Role::Fail, "Checkin failed")
                    .detail(format!("{e:#}"));
            }
        }
        result?
    };

    // The gateway's own string reaches exactly one display slot, this kv
    // value, folded through `cursor_safe` at the renderer so a response
    // cannot repaint the line describing it. The `-o json` payload below
    // carries the response verbatim; the fold is display-only.
    printer.kv("Server Status", &resp.status);
    printer.kv("Config Changed", resp.config_changed.to_string());

    if let Some(ref desired) = resp.desired_config {
        printer.status_simple(Role::Warn, "Server pushed desired config");
        let push_sec = printer.section("Server Config");
        match cfgd_core::state::save_pending_server_config(desired) {
            Ok(path) => {
                push_sec.status_simple(Role::Ok, format!("Saved to {}", path.posix()));
                push_sec.hint(MSG_RUN_APPLY);
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

    let all_drifts = cfgd_core::compliance::system_drifts(system_diffs.get_or_init(diff_system));

    let drift_status = if !all_drifts.is_empty() {
        // Same reasoning as the gateway checkin above, both halves:
        // `client.report_drift` narrates through the bare `&Printer` it's
        // handed and so needs a real section to inherit depth from, and the
        // wait itself is narrated one layer down, so this section writes only
        // the outcome.
        let drift_sec = printer.section("Drift");
        let _inherit = printer.depth_inheritance();
        let res = client
            .report_drift(&all_drifts, printer)
            .context("drift report to gateway failed");
        match &res {
            Ok(()) => {
                drift_sec.status_simple(
                    Role::Ok,
                    format!(
                        "{} reported",
                        cfgd_core::pluralize(all_drifts.len(), "drift item")
                    ),
                );
            }
            Err(e) => {
                drift_sec
                    .status(Role::Fail, "Drift report failed")
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
            config_explicit: false,
            profile: None,
            verbose: 0,
            quiet: true,
            no_color: true,
            color: crate::cli::ColorWhen::Auto,
            output: OutputFormatArg(OutputFormat::Table),
            list_envelope: false,
            theme: None,
            jsonpath: None,
            yes: false,
            state_dir: Some(state_dir.to_path_buf()),
            config_dir: None,
            cache_dir: None,
            runtime_dir: None,
            scope_arg: crate::cli::ScopeArg::User,
            command: None,
        }
    }

    // Linux-only because the fixture's drift source is the `gsettings`
    // configurator, which the registry registers only on Linux — on macOS
    // the declared drift has nothing to diff it, so the drift POST this
    // test counts never fires.
    #[cfg(target_os = "linux")]
    #[test]
    #[serial_test::serial]
    fn checkin_diffs_the_machine_once_for_both_its_compliance_snapshot_and_its_drift_report() {
        // `gsettings` stands in for every keyed configurator: the seam makes it
        // available and answers its bulk read, so the shim's log is the count
        // of times checkin asked the machine anything at all.
        let shim = cfgd_core::test_helpers::ToolShim::install(
            "CFGD_GSETTINGS_BIN",
            0,
            "org.gnome.cfgd-checkin color-scheme 'default'\n",
            "",
        );
        let config_dir = make_test_config_dir();
        std::fs::write(
            config_dir.path().join("cfgd.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  \
             profile: default\n  compliance:\n    enabled: true\n",
        )
        .unwrap();
        std::fs::write(
            config_dir.path().join("profiles").join("default.yaml"),
            r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: default
spec:
  system:
    gsettings:
      org.gnome.cfgd-checkin:
        color-scheme: prefer-dark
"#,
        )
        .unwrap();

        let state_dir = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(config_dir.path());
        let _state_env = EnvVarGuard::set("CFGD_STATE_DIR", state_dir.path().to_str().unwrap());

        let mut server = mockito::Server::new();
        let checkin = server
            .mock("POST", "/api/v1/checkin")
            .with_status(200)
            .with_body(r#"{"status":"ok","configChanged":false}"#)
            .create();
        // The declared value differs from what the shim reports, so the drift
        // report is REQUIRED — both consumers of the diff run in this test.
        let drift = server
            .mock(
                "POST",
                mockito::Matcher::Regex(r"/api/v1/devices/.*/drift".to_string()),
            )
            .with_status(200)
            .with_body("{}")
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

        assert!(result.is_ok(), "cmd_checkin should succeed: {result:?}");
        checkin.assert();
        drift.assert();
        assert_eq!(
            cap.json().expect("should emit structured Doc")["driftCount"].as_u64(),
            Some(1),
            "the drift report must carry the drift the compliance scan found"
        );
        assert_eq!(
            shim.argv_lines_naming("org.gnome.cfgd-checkin"),
            vec!["list-recursively org.gnome.cfgd-checkin"],
            "the compliance snapshot and the drift report share one diff pass"
        );
    }

    /// Every string in a gateway response is remote input, and this command
    /// echoes `status` verbatim into a kv row. An `ESC[2K` in it erases the
    /// line it is written on, so what a user reads is not what the gateway
    /// sent. The assertion covers the whole rendered command rather than one
    /// line of it, so a second slot that starts echoing the same string
    /// unfolded is caught here too.
    #[test]
    #[serial_test::serial]
    fn a_gateway_status_carrying_escapes_cannot_repaint_the_terminal() {
        let config_dir = make_test_config_dir();
        let state_dir = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(config_dir.path());
        let _state_env = EnvVarGuard::set("CFGD_STATE_DIR", state_dir.path().to_str().unwrap());

        let mut server = mockito::Server::new();
        let checkin = server
            .mock("POST", "/api/v1/checkin")
            .with_status(200)
            .with_body(r#"{"status":"\u001b[2Kok\u001b[31m","configChanged":false}"#)
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

        assert!(result.is_ok(), "cmd_checkin should succeed: {result:?}");
        checkin.assert();

        let human = cap.human();
        assert!(
            !human.contains("\x1b[2K") && !human.contains("\x1b[31m"),
            "a gateway escape reached the terminal: {human:?}"
        );
        let plain = cfgd_core::output::strip_ansi(&human);
        // Pin the VALUE against its own row: a bare `contains("ok")` matches
        // any line in the render carrying those two letters, so it would pass
        // with the status dropped entirely.
        let row = plain
            .lines()
            .map(str::trim)
            .find(|l| l.starts_with("Server Status"))
            .unwrap_or_else(|| panic!("the kv row must still render: {plain:?}"));
        assert_eq!(
            row.trim_start_matches("Server Status").trim(),
            "ok",
            "the row's whole value is the status, with the escapes gone: {row:?}"
        );
    }

    /// `client.report_drift` narrates through a bare `&Printer`, so its
    /// drift spinner used to render at depth 0 unconditionally. It now runs
    /// inside a real `printer.section("Drift")` plus `depth_inheritance()`,
    /// so its settled line nests one level deeper than the section header
    /// instead of sitting flush with it. Linux-only: the fixture's drift
    /// source is the `gsettings` configurator, registered only on Linux.
    #[cfg(target_os = "linux")]
    #[test]
    #[serial_test::serial]
    fn cmd_checkin_drift_settle_line_nests_under_the_drift_section_header() {
        let shim = cfgd_core::test_helpers::ToolShim::install(
            "CFGD_GSETTINGS_BIN",
            0,
            "org.gnome.cfgd-checkin color-scheme 'default'\n",
            "",
        );
        let config_dir = make_test_config_dir();
        std::fs::write(
            config_dir.path().join("cfgd.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  \
             profile: default\n  compliance:\n    enabled: true\n",
        )
        .unwrap();
        std::fs::write(
            config_dir.path().join("profiles").join("default.yaml"),
            r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: default
spec:
  system:
    gsettings:
      org.gnome.cfgd-checkin:
        color-scheme: prefer-dark
"#,
        )
        .unwrap();

        let state_dir = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(config_dir.path());
        let _state_env = EnvVarGuard::set("CFGD_STATE_DIR", state_dir.path().to_str().unwrap());

        let mut server = mockito::Server::new();
        let checkin = server
            .mock("POST", "/api/v1/checkin")
            .with_status(200)
            .with_body(r#"{"status":"ok","configChanged":false}"#)
            .create();
        let drift = server
            .mock(
                "POST",
                mockito::Matcher::Regex(r"/api/v1/devices/.*/drift".to_string()),
            )
            .with_status(200)
            .with_body("{}")
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
        let _ = shim;

        assert!(result.is_ok(), "cmd_checkin should succeed: {result:?}");
        checkin.assert();
        drift.assert();

        let human = cfgd_core::output::strip_ansi(&cap.human());
        crate::cli::test_support::assert_nests_under(&human, "Drift", "1 drift item reported");
    }

    // Linux-only like the drift tests above: the "never scanned" negative is
    // judged on the gsettings shim's log, and on a host that never registers
    // the gsettings configurator it would pass vacuously.
    #[cfg(target_os = "linux")]
    #[test]
    #[serial_test::serial]
    fn a_checkin_whose_gateway_call_fails_never_diffs_the_machine() {
        // Compliance is off, so the only consumer of the diff is the drift
        // report that runs AFTER the gateway answers. A 500 means it never
        // does — and the machine must not have been scanned for a report
        // nobody will read.
        let shim = cfgd_core::test_helpers::ToolShim::install(
            "CFGD_GSETTINGS_BIN",
            0,
            "org.gnome.cfgd-lazy color-scheme 'default'\n",
            "",
        );
        let config_dir = make_test_config_dir();
        std::fs::write(
            config_dir.path().join("profiles").join("default.yaml"),
            r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: default
spec:
  system:
    gsettings:
      org.gnome.cfgd-lazy:
        color-scheme: prefer-dark
"#,
        )
        .unwrap();

        let state_dir = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(config_dir.path());
        let _state_env = EnvVarGuard::set("CFGD_STATE_DIR", state_dir.path().to_str().unwrap());

        let mut server = mockito::Server::new();
        // The client retries a 5xx, so the count is "at least one attempt".
        let checkin = server
            .mock("POST", "/api/v1/checkin")
            .with_status(500)
            .with_body("boom")
            .expect_at_least(1)
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

        assert!(result.is_err(), "a 500 must fail the checkin");
        checkin.assert();
        assert!(
            shim.argv_lines_naming("org.gnome.cfgd-lazy").is_empty(),
            "the machine was scanned for a report the failed gateway call never sent: {}",
            shim.argv_log()
        );
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
            human.contains("Server Status"),
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

    /// `client.checkin` narrates through a bare `&Printer`
    /// (`status_simple`), so its gateway spinner used to render at depth 0
    /// unconditionally. It now runs inside a real `printer.section("Gateway")`
    /// plus `depth_inheritance()`, so its settled line nests one level
    /// deeper than the section header instead of sitting flush with it.
    #[test]
    #[serial_test::serial]
    fn cmd_checkin_gateway_settle_line_nests_under_the_gateway_section_header() {
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

        assert!(result.is_ok(), "cmd_checkin should succeed: {result:?}");
        mock.assert();

        let human = cfgd_core::output::strip_ansi(&cap.human());
        crate::cli::test_support::assert_nests_under(&human, "Gateway", "Checked in");
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

    /// The gateway tells one desired config from another by the hash checkin
    /// sends it, so a system setting a MODULE contributes has to be inside that
    /// hash. Read from the profile's own map it was not: a module could change
    /// what the machine is supposed to be and every checkin reported the same
    /// hash. The mock matches on the effective hash, so the profile-only one
    /// never reaches it.
    #[test]
    #[serial_test::serial]
    fn checkin_hashes_the_system_settings_a_module_contributes_not_the_profile_only_map() {
        let config_dir = make_test_config_dir();
        let module_dir = config_dir.path().join("modules").join("sysmod");
        std::fs::create_dir_all(&module_dir).unwrap();
        std::fs::write(
            module_dir.join("module.yaml"),
            r#"apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: sysmod
spec:
  system:
    sysctl:
      net.core.somaxconn: 8192
"#,
        )
        .unwrap();
        std::fs::write(
            config_dir.path().join("profiles").join("default.yaml"),
            r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: default
spec:
  modules: [sysmod]
"#,
        )
        .unwrap();

        let state_dir = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(config_dir.path());
        let _state_env = EnvVarGuard::set("CFGD_STATE_DIR", state_dir.path().to_str().unwrap());

        // Spelled out rather than read back through the merge under test: the
        // expectation is the map a reader of the two YAML files above would
        // write, so nothing derives the answer from the code being checked.
        let mut effective = cfgd_core::config::SystemSettings::new();
        effective.insert(
            "sysctl".to_string(),
            serde_yaml::from_str("net.core.somaxconn: 8192").unwrap(),
        );
        let profile_only = cfgd_core::config::SystemSettings::new();
        let expected_hash =
            cfgd_core::sha256_hex(serde_yaml::to_string(&effective).unwrap().as_bytes());
        let profile_only_hash =
            cfgd_core::sha256_hex(serde_yaml::to_string(&profile_only).unwrap().as_bytes());
        assert_ne!(
            expected_hash, profile_only_hash,
            "fixture is inert unless the module's settings change the hash"
        );

        let mut server = mockito::Server::new();
        let mock = server
            .mock("POST", "/api/v1/checkin")
            .match_body(mockito::Matcher::Regex(format!(
                "\"configHash\":\"{expected_hash}\""
            )))
            .with_status(200)
            .with_body(r#"{"status":"ok","configChanged":false}"#)
            .create();
        // Whether the module's sysctl value is drifted on THIS host decides
        // whether checkin also posts a drift report, so the endpoint is answered
        // without being required — the subject here is the hash, not the host.
        let _drift = server
            .mock(
                "POST",
                mockito::Matcher::Regex(r"/api/v1/devices/.*/drift".to_string()),
            )
            .with_status(200)
            .with_body("{}")
            .expect_at_least(0)
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
            result.is_ok(),
            "checkin should have sent the effective-map hash: {result:?}"
        );
        mock.assert();
    }
}
