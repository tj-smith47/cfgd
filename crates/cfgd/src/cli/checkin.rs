use super::*;

use anyhow::Context;
use cfgd_core::PathDisplayExt;
use cfgd_core::output::{Doc, Printer, Role};
use cfgd_core::server_client::{DeviceCredential, ServerClient};

// no-header-ok: the verdict is about what the gateway accepted, and the
// machine identity it reports is the header a reader of this verb needs.
pub fn cmd_checkin(
    cli: &Cli,
    printer: &Printer,
    server_url: &str,
    api_key: Option<&str>,
    device_id: Option<&str>,
) -> anyhow::Result<()> {
    // heading-first-ok: the gateway round-trip narrates one layer down, under
    // the Gateway section that reports its verdict
    printer.heading("Checkin");

    let ctx = RunContext::new(cli, printer);
    let (cfg, profile_name, local_resolved) = ctx.config_and_profile()?;
    let config_dir = ctx.config_dir();

    // The same resolution `cfgd compliance` collects against, so the compliance
    // report, the hash and the drift scan below all read the source-composed
    // desired state that `apply` writes.
    let mut inputs =
        super::compliance::ComplianceInputs::of_config(&ctx, cfg, local_resolved, printer)?;
    // A manifest that cannot be read leaves its packages out of the declared
    // set. Reported anyway, the version map would retire every version that
    // manifest declares and the compliance checks would call its packages
    // undeclared, so the check-in withholds both and still goes out.
    let manifest_error = inputs.take_manifest_error();
    if let Some(ref e) = manifest_error {
        tracing::warn!(
            error = %e,
            "checkin: a package manifest could not be read — the check-in withholds package versions and compliance"
        );
    }
    let registry = &inputs.registry;
    let resolved = &inputs.resolved;
    let resolved_modules = &inputs.modules;

    let stored_cred = cfgd_core::server_client::load_credential().ok().flatten();
    let client = build_checkin_client(server_url, api_key, device_id, stored_cred.as_ref());

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
        cfgd_core::compliance::collect_system_diffs(&resolved.merged, resolved_modules, registry)
    };

    // Kept whole for the length of the check-in, because the report borrows
    // its rows from it.
    let compliance_snapshot = match cfg
        .spec
        .compliance
        .as_ref()
        .filter(|c| c.enabled && manifest_error.is_none())
    {
        Some(compliance_cfg) => {
            let checkin_state = ctx.state()?;
            match inputs.collect(
                profile_name,
                config_dir,
                &compliance_cfg.scope,
                &[],
                printer,
                checkin_state,
                Some(system_diffs.get_or_init(diff_system)),
            ) {
                Ok(snapshot) => {
                    printer.kv("Compliance", snapshot.summary.counts_line());
                    Some(snapshot)
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to collect compliance snapshot for checkin");
                    None
                }
            }
        }
        None => None,
    };

    // What the machine reports about itself beyond the hash: the versions it
    // holds for the packages it DECLARES, and which layer owns each backup
    // unit's schedule. Both are questions only the device can answer and the
    // check-in is its only channel to the cluster, where a `ConfigPolicy`
    // version pin and a `BackupPolicy` schedule projection are decided from
    // them. Composed by the one builder the daemon's check-in uses too.
    //
    // Nothing observed means nothing claimed, whether the whole context failed
    // to build or one manager holding declared packages could not be listed:
    // the map is left out of the body entirely, the gateway's own manager for
    // it writes nothing, and the cluster keeps the versions the last check-in
    // that could look reported.
    let pkg_cx = match manifest_error {
        Some(_) => None,
        None => ctx
            .package_context()
            .inspect_err(|e| tracing::warn!(error = %e, "checkin: package versions unavailable"))
            .ok(),
    };
    let checkin_facts = cfgd_core::server_client::CheckinFacts::collect(
        &resolved.merged,
        resolved_modules,
        registry,
        pkg_cx.as_ref(),
        compliance_snapshot.as_ref(),
    )?;

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
        //
        // heading-first-ok: the section reports the round-trip it opened, and
        // `checkin` narrates through the printer it is handed
        let gateway_sec = printer.section("Gateway");
        let _inherit = printer.depth_inheritance();
        let result = client
            .checkin(checkin_facts, printer)
            .context("checkin to gateway failed");
        match &result {
            Ok(resp) => {
                // The VERDICT of the round-trip, with the server's own status
                // string as its detail: that string is what the round-trip
                // PRODUCED, stated once, on the row that produced it. The
                // detail slot folds through `cursor_safe` at the renderer, so
                // a response cannot repaint the line describing it; the
                // `-o json` payload below carries it verbatim.
                gateway_sec.status(Role::Ok, "Checked in").detail(format!(
                    "server status {}, config {}",
                    resp.status,
                    if resp.config_changed {
                        "changed"
                    } else {
                        "unchanged"
                    }
                ));
            }
            Err(e) => {
                gateway_sec
                    .status(Role::Fail, "Checkin failed")
                    .detail(format!("{e:#}"));
            }
        }
        result?
    };

    // The cadences the cluster owns for this machine, recorded where the
    // daemon's timers and `cfgd backup list` both read them. Never written to
    // the profile on disk: it is the cluster's answer, replaced by the next
    // check-in. An answer that carries no projection at all is a gateway that
    // could not read the cluster, so the set already recorded stands.
    if let Some(ref projections) = resp.backup_schedules {
        match ctx.state() {
            Ok(state) => {
                cfgd_core::backup::record_cluster_schedules(state, projections);
            }
            Err(e) => {
                tracing::warn!(error = %e, "checkin: state store unavailable — the cluster-owned backup schedules were not recorded");
            }
        }
    }

    if let Some(ref desired) = resp.desired_config {
        printer.status_simple(Role::Warn, "Server pushed desired config");
        let push_sec = printer.section("Server Config");
        match cfgd_core::state::save_pending_server_config(desired) {
            Ok(path) => {
                push_sec.status_simple(
                    Role::Ok,
                    format!(
                        "Saved to {}",
                        cfgd_core::fold_home_in_text(&path.display_posix())
                    ),
                );
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

    // The section names the CLASS this command can report, because that is all
    // the walk above collects: `system_drifts` pairs the answers of the
    // available system CONFIGURATORS with their reporters and gathers nothing
    // else. A package, file or env finding never reaches the gateway, so a
    // section headed `Drift` promised the fleet a machine-wide verdict this
    // payload has never carried.
    let drift_status = if !all_drifts.is_empty() {
        // Same reasoning as the gateway checkin above, both halves:
        // `client.report_drift` narrates through the bare `&Printer` it's
        // handed and so needs a real section to inherit depth from, and the
        // wait itself is narrated one layer down, so this section writes only
        // the outcome.
        let drift_sec = printer.section("System Settings");
        let _inherit = printer.depth_inheritance();
        let res = client
            .report_drift(&all_drifts, printer)
            .context("system settings drift report to gateway failed");
        match &res {
            Ok(()) => {
                drift_sec.status_simple(
                    Role::Ok,
                    format!(
                        "Reported {}",
                        cfgd_core::pluralize(all_drifts.len(), "drifted system setting")
                    ),
                );
            }
            Err(e) => {
                drift_sec
                    .status(Role::Fail, "System settings drift report failed")
                    .detail(format!("{e:#}"));
            }
        }
        res?;
        "drift_reported"
    } else {
        // Inside the same section as the reported branch: which section a fact
        // lands in cannot depend on whether the fact is empty.
        let drift_sec = printer.section("System Settings");
        drift_sec.status_simple(Role::Info, "No system settings drift to report");
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
        && cfgd_core::server_client::credential_matches(server_url, cred)
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

    fn test_facts() -> cfgd_core::server_client::CheckinFacts<'static> {
        cfgd_core::server_client::CheckinFacts {
            hostname: "test-host".into(),
            config_hash: "hash".into(),
            compliance: None,
            package_versions: None,
            backup_schedule_owners: None,
        }
    }

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
        let result = client.checkin(test_facts(), &printer);

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
        let result = client.checkin(test_facts(), &printer);

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
        let result = client.checkin(test_facts(), &printer);

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
        let result = client.checkin(test_facts(), &printer);

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
            hints: false,
            no_hints: false,
            theme: None,
            mask_env_values: None,
            migration_policy: None,
            update_policy: None,
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
            crate::seams::GSETTINGS_BIN_ENV,
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
        let _state_env = EnvVarGuard::set(
            cfgd_core::CFGD_STATE_DIR_ENV,
            state_dir.path().to_str().unwrap(),
        );

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

    /// The check-in reports the machine's failing checks as `cfgd compliance`
    /// collects them for the same config: the same rows, the same subjects
    /// and the same details, so the fleet and the machine never disagree
    /// about why it is out of compliance.
    #[test]
    #[serial_test::serial]
    fn checkin_reports_the_failing_checks_cfgd_compliance_collects() {
        let config_dir = make_test_config_dir();
        let root = config_dir.path();
        std::fs::create_dir_all(root.join("files")).unwrap();
        std::fs::write(root.join("files").join("zshrc"), "export A=1\n").unwrap();
        std::fs::write(
            root.join("cfgd.yaml"),
            format!(
                "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  \
                 profile: default\n  compliance:\n    enabled: true\n    scope:\n      \
                 packages: false\n      system: false\n      secrets: false\n      \
                 watchPaths: [{}]\n",
                cfgd_core::to_posix_string(root.join("watched"))
            ),
        )
        .unwrap();
        std::fs::write(
            root.join("profiles").join("default.yaml"),
            format!(
                "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\n\
                 spec:\n  files:\n    managed:\n      - source: files/zshrc\n        \
                 target: {}\n",
                cfgd_core::to_posix_string(root.join(".zshrc"))
            ),
        )
        .unwrap();

        let state_dir = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(root);
        let _state_env = EnvVarGuard::set(
            cfgd_core::CFGD_STATE_DIR_ENV,
            state_dir.path().to_str().unwrap(),
        );
        let cli = test_cli_for(root, state_dir.path());

        let (quiet, _) = Printer::for_test_doc();
        let (_, collected) = super::super::compliance::collect_and_store_compliance_snapshot(
            &RunContext::new(&cli, &quiet),
        )
        .expect("cfgd compliance collects");
        let report = cfgd_core::server_client::CheckinCompliance::from_snapshot(&collected);
        assert_eq!(
            report
                .checks
                .iter()
                .map(|c| (c.category, c.status))
                .collect::<Vec<_>>(),
            vec![
                ("file", cfgd_core::compliance::ComplianceStatus::Violation),
                (
                    "watchPath",
                    cfgd_core::compliance::ComplianceStatus::Warning
                ),
            ],
            "the fixture fails one check of each kind"
        );
        let expected = serde_json::json!({ "complianceSummary": report }).to_string();

        let mut server = mockito::Server::new();
        let checkin = server
            .mock("POST", "/api/v1/checkin")
            .match_body(mockito::Matcher::PartialJsonString(expected))
            .with_status(200)
            .with_body(r#"{"status":"ok","configChanged":false}"#)
            .create();

        let (printer, _cap) = Printer::for_test_doc();
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
        let _state_env = EnvVarGuard::set(
            cfgd_core::CFGD_STATE_DIR_ENV,
            state_dir.path().to_str().unwrap(),
        );

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
            .find(|l| l.contains("Checked in"))
            .unwrap_or_else(|| panic!("the verdict row must still render: {plain:?}"));
        assert!(
            row.ends_with("server status ok, config unchanged"),
            "the row's detail is the status, with the escapes gone: {row:?}"
        );
    }

    /// `client.report_drift` narrates through a bare `&Printer`, so its
    /// drift spinner used to render at depth 0 unconditionally. It now runs
    /// inside a real `printer.section("System Settings")` plus
    /// `depth_inheritance()`, so its settled line nests one level deeper than the
    /// section header instead of sitting flush with it. The section names the
    /// class rather than `Drift`, because the gateway is only ever told about
    /// system settings. Linux-only: the fixture's drift source is the
    /// `gsettings` configurator, registered only on Linux.
    #[cfg(target_os = "linux")]
    #[test]
    #[serial_test::serial]
    fn cmd_checkin_drift_settle_line_nests_under_the_system_settings_section_header() {
        let shim = cfgd_core::test_helpers::ToolShim::install(
            crate::seams::GSETTINGS_BIN_ENV,
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
        let _state_env = EnvVarGuard::set(
            cfgd_core::CFGD_STATE_DIR_ENV,
            state_dir.path().to_str().unwrap(),
        );

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
        crate::cli::test_support::assert_nests_under(
            &human,
            "System Settings",
            "Reported 1 drifted system setting",
        );
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
            crate::seams::GSETTINGS_BIN_ENV,
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
        let _state_env = EnvVarGuard::set(
            cfgd_core::CFGD_STATE_DIR_ENV,
            state_dir.path().to_str().unwrap(),
        );

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
        let _state_env = EnvVarGuard::set(
            cfgd_core::CFGD_STATE_DIR_ENV,
            state_dir.path().to_str().unwrap(),
        );

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
            human.contains("Checked in") && human.contains("server status ok"),
            "the gateway's answer rides on the Checked in row, got: {human}"
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
        let _state_env = EnvVarGuard::set(
            cfgd_core::CFGD_STATE_DIR_ENV,
            state_dir.path().to_str().unwrap(),
        );

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
        let _state_env = EnvVarGuard::set(
            cfgd_core::CFGD_STATE_DIR_ENV,
            state_dir.path().to_str().unwrap(),
        );

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

    /// The daemon's periodic check-in and `cfgd checkin` are two callers in two
    /// crates, and a machine gets one behaviour or the other depending on which
    /// ran last. Given ONE pushed configuration, both must leave the same
    /// pending file with the same bytes.
    #[test]
    #[serial_test::serial]
    fn both_check_in_paths_record_the_same_pushed_configuration() {
        use cfgd_core::config::*;

        const PUSHED: &str =
            r#"{"status":"ok","configChanged":true,"desiredConfig":{"packages":["git","curl"]}}"#;
        let pending_of = |state_dir: &std::path::Path| {
            std::fs::read_to_string(state_dir.join("pending-server-config.json"))
                .expect("the pushed configuration was recorded")
        };

        // The CLI's path.
        let cli_config = make_test_config_dir();
        let cli_state = tempfile::tempdir().unwrap();
        let cli_written = {
            let _home = cfgd_core::with_test_home_guard(cli_config.path());
            let _state_env = EnvVarGuard::set(
                cfgd_core::CFGD_STATE_DIR_ENV,
                cli_state.path().to_str().unwrap(),
            );
            let mut server = mockito::Server::new();
            let _mock = server
                .mock("POST", "/api/v1/checkin")
                .with_status(200)
                .with_body(PUSHED)
                .create();
            let cli = test_cli_for(cli_config.path(), cli_state.path());
            let (printer, _cap) = Printer::for_test_doc();
            cmd_checkin(
                &cli,
                &printer,
                &server.url(),
                Some("test-key"),
                Some("dev-1"),
            )
            .expect("the gateway answered");
            pending_of(cli_state.path())
        };

        // The daemon's path, against the same answer.
        let daemon_home = tempfile::tempdir().unwrap();
        let daemon_written = {
            let _home = cfgd_core::with_test_home_guard(daemon_home.path());
            let _state_env = EnvVarGuard::set(
                cfgd_core::CFGD_STATE_DIR_ENV,
                daemon_home.path().to_str().unwrap(),
            );
            let mut server = mockito::Server::new();
            let _mock = server
                .mock("POST", "/api/v1/checkin")
                .with_status(200)
                .with_body(PUSHED)
                .create();
            cfgd_core::server_client::save_credential(
                &cfgd_core::server_client::DeviceCredential {
                    server_url: server.url(),
                    device_id: "dev-1".to_string(),
                    api_key: "test-key".to_string(),
                    username: "tester".to_string(),
                    team: None,
                    enrolled_at: cfgd_core::utc_now_iso8601(),
                },
            )
            .expect("store the device credential");

            let config = CfgdConfig {
                api_version: cfgd_core::API_VERSION.into(),
                kind: "Config".into(),
                metadata: ConfigMetadata {
                    name: "test".into(),
                },
                spec: ConfigSpec {
                    profile: Some("default".into()),
                    origin: vec![OriginSpec {
                        origin_type: OriginType::Server,
                        url: server.url(),
                        branch: "main".into(),
                        auth: None,
                        ssh_strict_host_key_checking: Default::default(),
                    }],
                    ..Default::default()
                },
                deprecations: Vec::new(),
                legacy_output_keys: Vec::new(),
            };
            cfgd_core::daemon::try_server_checkin(
                &config,
                &cfgd_core::test_helpers::test_printer(),
                || Some(test_facts()),
            );
            pending_of(daemon_home.path())
        };

        assert_eq!(
            cli_written, daemon_written,
            "one pushed configuration, one recording, whichever caller checked in"
        );
    }

    #[test]
    #[serial_test::serial]
    fn cmd_checkin_server_500_returns_err() {
        let config_dir = make_test_config_dir();
        let state_dir = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(config_dir.path());
        let _state_env = EnvVarGuard::set(
            cfgd_core::CFGD_STATE_DIR_ENV,
            state_dir.path().to_str().unwrap(),
        );

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
        let _state_env = EnvVarGuard::set(
            cfgd_core::CFGD_STATE_DIR_ENV,
            state_dir.path().to_str().unwrap(),
        );

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

    /// One enrolled machine for both senders: a source whose file sits
    /// outside its allowed paths, compliance on, a `Cargo.toml` manifest
    /// declaring `ripgrep`, and a gateway that records every check-in body.
    struct TwoSenderMachine {
        cli: crate::cli::Cli,
        root: std::path::PathBuf,
        state_dir: std::path::PathBuf,
        cache_dir: std::path::PathBuf,
        destination: String,
        server: mockito::ServerGuard,
        posted: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
        _guards: (
            EnvVarGuard,
            EnvVarGuard,
            cfgd_core::TestHomeGuard,
            Box<dyn std::any::Any>,
        ),
    }

    impl TwoSenderMachine {
        fn new() -> Self {
            let allow = EnvVarGuard::set(cfgd_core::CFGD_ALLOW_LOCAL_SOURCES_ENV, "1");
            let (workspace, config_dir, state_dir, destination) =
                cfgd_test_fixtures::violating_backup_source_setup();
            let cache_dir = tempfile::tempdir().unwrap();
            let root = config_dir.path().to_path_buf();
            let home = cfgd_core::with_test_home_guard(&root);
            let state_env = EnvVarGuard::set(
                cfgd_core::CFGD_STATE_DIR_ENV,
                state_dir.path().to_str().unwrap(),
            );

            let posted =
                std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
            let sink = std::sync::Arc::clone(&posted);
            let mut server = mockito::Server::new();
            let checkin = server
                .mock("POST", "/api/v1/checkin")
                .expect(2)
                .with_status(200)
                .with_body_from_request(move |request| {
                    let body = request.body().expect("a check-in body");
                    sink.lock()
                        .unwrap()
                        .push(serde_json::from_slice(body).expect("a JSON check-in"));
                    br#"{"status":"ok","configChanged":false}"#.to_vec()
                })
                .create();

            let config = std::fs::read_to_string(root.join("cfgd.yaml")).unwrap();
            std::fs::write(
                root.join("cfgd.yaml"),
                format!(
                    "{config}  compliance:\n    enabled: true\n  origin:\n    - type: Server\n      \
                     url: {}\n",
                    server.url()
                ),
            )
            .unwrap();
            std::fs::write(
                root.join("Cargo.toml"),
                "[package]\nname = \"t\"\nversion = \"0.1.0\"\n\n[dependencies]\nripgrep = \"14\"\n",
            )
            .unwrap();
            std::fs::write(
                root.join("profiles").join("default.yaml"),
                "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  \
                 packages:\n    cargo:\n      file: Cargo.toml\n",
            )
            .unwrap();
            cfgd_core::server_client::save_credential(&make_cred(
                &server.url(),
                "dev-1",
                "test-key",
            ))
            .expect("store the device credential");

            let mut cli = test_cli_for(&root, state_dir.path());
            cli.cache_dir = Some(cache_dir.path().to_path_buf());
            let (quiet, _) = Printer::for_test_doc();
            crate::cli::sync::cmd_sync(&cli, &quiet).expect("the source syncs into the cache");

            Self {
                cli,
                state_dir: state_dir.path().to_path_buf(),
                cache_dir: cache_dir.path().to_path_buf(),
                root,
                destination,
                server,
                posted,
                _guards: (
                    allow,
                    state_env,
                    home,
                    Box::new((workspace, config_dir, state_dir, cache_dir, checkin)),
                ),
            }
        }

        fn run_cfgd_checkin(&self) {
            let (printer, _cap) = Printer::for_test_doc();
            cmd_checkin(
                &self.cli,
                &printer,
                &self.server.url(),
                Some("test-key"),
                Some("dev-1"),
            )
            .expect("cfgd checkin succeeds");
        }

        fn run_daemon_ticks(&self, hooks: std::sync::Arc<dyn cfgd_core::daemon::DaemonHooks>) {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(cfgd_core::daemon::run_compliance_and_reconcile_ticks(
                    &self.root.join("cfgd.yaml"),
                    hooks,
                    &self.state_dir,
                    Some(&self.cache_dir),
                ))
                .expect("the daemon ticks");
        }

        /// Every body posted, after the gateway saw exactly the two it expects.
        fn posted(&self) -> Vec<serde_json::Value> {
            let posted = self.posted.lock().unwrap().clone();
            assert_eq!(posted.len(), 2, "one check-in from each sender: {posted:?}");
            posted
        }
    }

    /// The daemon and `cfgd checkin` compose one check-in for one machine: a
    /// source-constraint violation reaches the gateway as a failing check and
    /// a package a `Cargo.toml` declares reaches it with its version, from
    /// both senders, under the same hash and hostname.
    #[test]
    #[serial_test::serial]
    fn the_daemon_and_cfgd_checkin_post_the_same_check_in() {
        let machine = TwoSenderMachine::new();
        let _cargo = cfgd_core::test_helpers::ToolShim::install(
            &crate::seams::tool_seam_var("cargo"),
            0,
            "ripgrep v14.1.0:\n    rg\n",
            "",
        );
        machine.run_cfgd_checkin();
        machine.run_daemon_ticks(std::sync::Arc::new(
            super::super::registry::WorkstationDaemonHooks,
        ));

        let posted = machine.posted();
        let destination = &machine.destination;
        let expected_checks = serde_json::json!([{
            "category": "source-constraint",
            "name": destination,
            "status": "Violation",
            "detail": format!("path '{destination}' not in allowed paths for source 'acme'"),
        }]);
        for (sender, body) in ["cfgd checkin", "the daemon"].iter().zip(&posted) {
            assert_eq!(
                body["complianceSummary"]["checks"], expected_checks,
                "{sender} reports the source-constraint violation as a failing check"
            );
            assert_eq!(
                body["packageVersions"],
                serde_json::json!({ "cargo/ripgrep": "14.1.0" }),
                "{sender} reports the package the manifest declares"
            );
            assert_eq!(body["hostname"], cfgd_core::hostname_string().as_str());
        }
        assert_eq!(
            posted[0], posted[1],
            "the two senders post one check-in, hash included"
        );
    }

    /// The workstation hooks, except that the manifest breaks right after the
    /// first read: the daemon's compliance tick collects its snapshot from the
    /// good manifest, and every later reader finds it unreadable.
    struct ManifestBreaksAfterFirstRead {
        manifest: std::path::PathBuf,
        reads: std::sync::atomic::AtomicUsize,
    }

    impl cfgd_core::daemon::DaemonHooks for ManifestBreaksAfterFirstRead {
        fn build_registry(
            &self,
            config: &cfgd_core::config::CfgdConfig,
        ) -> cfgd_core::providers::ProviderRegistry {
            super::super::registry::WorkstationDaemonHooks.build_registry(config)
        }
        fn plan_files(
            &self,
            config_dir: &std::path::Path,
            resolved: &cfgd_core::config::ResolvedProfile,
        ) -> cfgd_core::errors::Result<Vec<cfgd_core::providers::FileAction>> {
            super::super::registry::WorkstationDaemonHooks.plan_files(config_dir, resolved)
        }
        fn plan_files_with_manager(
            &self,
            config_dir: &std::path::Path,
            resolved: &cfgd_core::config::ResolvedProfile,
        ) -> cfgd_core::errors::Result<cfgd_core::daemon::PlannedFiles> {
            super::super::registry::WorkstationDaemonHooks
                .plan_files_with_manager(config_dir, resolved)
        }
        fn plan_packages(
            &self,
            profile: &cfgd_core::config::MergedProfile,
            managers: &[&dyn cfgd_core::providers::PackageManager],
            cfgd_installed: &std::collections::HashSet<String>,
            cx: &cfgd_core::providers::PackageContext<'_>,
        ) -> cfgd_core::errors::Result<Vec<cfgd_core::providers::PackageAction>> {
            super::super::registry::WorkstationDaemonHooks.plan_packages(
                profile,
                managers,
                cfgd_installed,
                cx,
            )
        }
        fn plan_packages_observed(
            &self,
            profile: &cfgd_core::config::MergedProfile,
            managers: &[&dyn cfgd_core::providers::PackageManager],
            cfgd_installed: &std::collections::HashSet<String>,
            cx: &cfgd_core::providers::PackageContext<'_>,
        ) -> cfgd_core::errors::Result<(
            Vec<cfgd_core::providers::PackageAction>,
            cfgd_core::reconciler::ActualPackages,
        )> {
            super::super::registry::WorkstationDaemonHooks.plan_packages_observed(
                profile,
                managers,
                cfgd_installed,
                cx,
            )
        }
        fn extend_registry_custom_managers(
            &self,
            registry: &mut cfgd_core::providers::ProviderRegistry,
            packages: &cfgd_core::config::PackagesSpec,
        ) {
            super::super::registry::WorkstationDaemonHooks
                .extend_registry_custom_managers(registry, packages)
        }
        fn build_file_manager(
            &self,
            config_dir: &std::path::Path,
            resolved: &cfgd_core::config::ResolvedProfile,
        ) -> cfgd_core::errors::Result<Option<Box<dyn cfgd_core::providers::FileManager>>> {
            super::super::registry::WorkstationDaemonHooks.build_file_manager(config_dir, resolved)
        }
        fn resolve_manifest_packages(
            &self,
            config_dir: &std::path::Path,
            merged: &mut cfgd_core::config::MergedProfile,
        ) -> cfgd_core::errors::Result<()> {
            if self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) > 0 {
                std::fs::write(&self.manifest, "[dependencies\nripgrep = ").unwrap();
            }
            super::super::registry::WorkstationDaemonHooks
                .resolve_manifest_packages(config_dir, merged)
        }
        fn expand_tilde(&self, path: &std::path::Path) -> std::path::PathBuf {
            super::super::registry::WorkstationDaemonHooks.expand_tilde(path)
        }
        fn prune_orphaned_packages(
            &self,
            orphans: &[cfgd_core::providers::OrphanedPackage],
            cx: &cfgd_core::providers::PackageContext<'_>,
        ) -> Vec<(String, String)> {
            super::super::registry::WorkstationDaemonHooks.prune_orphaned_packages(orphans, cx)
        }
    }

    /// A manifest that cannot be read withholds both the package versions and
    /// the compliance summary, from both senders. The daemon holds a snapshot
    /// its compliance tick took while the manifest still read, and that
    /// snapshot is withheld with the versions: it would report packages the
    /// machine can no longer say it declares.
    #[test]
    #[serial_test::serial]
    fn an_unreadable_manifest_withholds_versions_and_compliance_from_both_senders() {
        let machine = TwoSenderMachine::new();
        let _cargo = cfgd_core::test_helpers::ToolShim::install(
            &crate::seams::tool_seam_var("cargo"),
            0,
            "ripgrep v14.1.0:\n    rg\n",
            "",
        );
        let hooks = std::sync::Arc::new(ManifestBreaksAfterFirstRead {
            manifest: machine.root.join("Cargo.toml"),
            reads: std::sync::atomic::AtomicUsize::new(0),
        });
        machine.run_daemon_ticks(hooks.clone());
        assert!(
            hooks.reads.load(std::sync::atomic::Ordering::SeqCst) >= 2,
            "the compliance tick read the manifest before it broke, and the check-in after"
        );
        machine.run_cfgd_checkin();

        for (sender, body) in ["the daemon", "cfgd checkin"].iter().zip(&machine.posted()) {
            assert_eq!(body["deviceId"], "dev-1", "{sender} checked in: {body}");
            assert!(
                body.get("packageVersions").is_none(),
                "{sender} withholds package versions: {body}"
            );
            assert!(
                body.get("complianceSummary").is_none(),
                "{sender} withholds the compliance summary: {body}"
            );
        }
    }
}
