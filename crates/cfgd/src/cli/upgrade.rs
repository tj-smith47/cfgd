use cfgd_core::PathDisplayExt;
use cfgd_core::format_bytes;
use cfgd_core::output::{Doc, Printer, Role};

/// Report a completed upgrade — the human render and the structured payload
/// together — for both install paths (`cfgd upgrade` and the startup policy
/// update).
///
/// The daemon fact is minted HERE, on both channels at once, so the two cannot
/// disagree: a call site that could emit the payload without the line is what
/// let the `-o json` field and the human render describe the same event
/// differently. `terminated`, not `restarted`, because that is all
/// `terminate_daemon_if_running` does — a service manager brings a managed
/// daemon back, while one started by hand stays down and reconciles nothing
/// until the user starts it again, which is exactly the thing the reader has to
/// act on.
/// Takes the payload as PAIRS rather than a `Value`, so the daemon key always
/// has somewhere to land: handed a `Value`, an array or a scalar would take the
/// insert silently and ship a human line the payload does not carry — the
/// disagreement this builder exists to make unrepresentable.
fn upgraded_doc(
    version: &str,
    installed_path: String,
    daemon_terminated: bool,
    data: impl IntoIterator<Item = (&'static str, serde_json::Value)>,
) -> Doc {
    let mut doc = Doc::new()
        .status(Role::Ok, format!("cfgd upgraded to {version}"))
        .kv("Installed to", installed_path);
    if daemon_terminated {
        doc = doc.kv("Daemon", "terminated to pick up the new binary");
    }
    let mut payload: serde_json::Map<String, serde_json::Value> = data
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect();
    payload.insert("daemonTerminated".into(), daemon_terminated.into());
    doc.with_data(serde_json::Value::Object(payload))
}

pub fn cmd_upgrade(
    printer: &Printer,
    config_path: &std::path::Path,
    check_only: bool,
    require_cosign: bool,
) -> anyhow::Result<()> {
    use cfgd_core::config;
    use cfgd_core::upgrade;

    // The effective update config supplies the release channel for the version
    // check and gates the user-scope skill ride-along that `install_release`
    // runs after a successful install (no second prompt).
    let update_cfg = match config::load_config(config_path) {
        Ok(mut c) => {
            crate::cli::helpers::drain_config_deprecations(printer, &mut c);
            c.spec.update.unwrap_or_default()
        }
        Err(_) => Default::default(),
    };
    let channel = update_cfg.channel.as_deref();

    if check_only {
        let check = upgrade::check_latest(env!("CARGO_PKG_VERSION"), None, channel, Some(printer))
            .map_err(|e| {
                let msg = format!("Failed to check latest version: {e}");
                crate::cli::cli_error_ctx(
                    e.into(),
                    env!("CARGO_PKG_VERSION"),
                    "check_failed",
                    msg,
                    serde_json::json!({ "currentVersion": env!("CARGO_PKG_VERSION") }),
                )
            })?;

        if check.update_available {
            printer.emit(
                Doc::new()
                    .status(
                        Role::Info,
                        format!(
                            "Update available: {} {} {}",
                            check.current,
                            printer.arrow(),
                            check.latest
                        ),
                    )
                    .hint("Run 'cfgd upgrade' to install")
                    .with_data(serde_json::json!({
                        "currentVersion": check.current.to_string(),
                        "latestVersion": check.latest.to_string(),
                        "updateAvailable": true,
                    })),
            );
            // "Action needed, not an error" — reserves Error (1) for
            // actual failures so scripts can distinguish `--check`
            // results from network/IO errors.
            cfgd_core::exit::ExitCode::UpdateAvailable.exit();
        } else {
            printer.emit(
                Doc::new()
                    .status(Role::Ok, format!("cfgd {} is up to date", check.current))
                    .with_data(serde_json::json!({
                        "currentVersion": check.current.to_string(),
                        "latestVersion": check.latest.to_string(),
                        "updateAvailable": false,
                    })),
            );
        }

        return Ok(());
    }

    printer.heading("Upgrade");

    let check = upgrade::check_latest(env!("CARGO_PKG_VERSION"), None, channel, Some(printer))
        .map_err(|e| {
            let msg = format!("Failed to check latest version: {e}");
            crate::cli::cli_error_ctx(
                e.into(),
                env!("CARGO_PKG_VERSION"),
                "check_failed",
                msg,
                serde_json::json!({ "currentVersion": env!("CARGO_PKG_VERSION") }),
            )
        })?;

    if !check.update_available {
        printer.emit(
            Doc::new()
                .status(
                    Role::Ok,
                    format!("cfgd {} is already the latest version", check.current),
                )
                .with_data(serde_json::json!({
                    "currentVersion": check.current.to_string(),
                    "targetVersion": check.current.to_string(),
                    "downloaded": false,
                    "installed": false,
                    "verified": false,
                    "updateAvailable": false,
                    // No install was performed → no verification ran. Surface
                    // null so structured consumers can distinguish "skipped"
                    // from a real "sha256-only" or "cosign" run.
                    "verificationMode": serde_json::Value::Null,
                })),
        );
        return Ok(());
    }

    let release = check.release.as_ref().ok_or_else(|| {
        crate::cli::cli_error(
            check.latest.to_string(),
            "no_release",
            "release info not available".to_string(),
            serde_json::json!({
                "currentVersion": check.current.to_string(),
                "latestVersion": check.latest.to_string(),
            }),
        )
    })?;

    let asset = upgrade::find_asset_for_platform(release).map_err(|e| {
        let msg = format!("no asset for platform: {e}");
        crate::cli::cli_error_ctx(
            e.into(),
            check.latest.to_string(),
            "no_release",
            msg,
            serde_json::json!({
                "currentVersion": check.current.to_string(),
                "latestVersion": check.latest.to_string(),
            }),
        )
    })?;

    {
        let sec = printer.section("Update Available");
        sec.kv(
            "Version",
            format!("{} {} {}", check.current, printer.arrow(), check.latest),
        );
        sec.kv("Binary", &asset.name);
        if asset.size > 0 {
            sec.kv("Size", format_bytes(asset.size));
        }
    }

    let applied = upgrade::install_release(
        release,
        asset,
        require_cosign,
        &update_cfg,
        env!("CARGO_PKG_VERSION"),
        Some(printer),
    )
    .map_err(|e| {
        // Strict-cosign failures get a distinct error kind so structured
        // consumers can route them differently from generic install
        // failures (network, disk, archive corruption).
        let kind = if matches!(
            &e,
            cfgd_core::errors::CfgdError::Upgrade(
                cfgd_core::errors::UpgradeError::CosignRequired { .. }
            )
        ) {
            "cosign_required"
        } else {
            "install_failed"
        };
        let msg = format!("download/install failed: {e}");
        crate::cli::cli_error_ctx(
            e.into(),
            check.latest.to_string(),
            kind,
            msg,
            serde_json::json!({
                "currentVersion": check.current.to_string(),
                "latestVersion": check.latest.to_string(),
                "requireCosign": require_cosign,
            }),
        )
    })?;
    let report = &applied.report;

    printer.emit(upgraded_doc(
        &check.latest.to_string(),
        report.installed_path.display_posix(),
        applied.daemon_terminated,
        [
            (
                "currentVersion",
                serde_json::json!(check.current.to_string()),
            ),
            ("targetVersion", serde_json::json!(check.latest.to_string())),
            ("downloaded", serde_json::json!(true)),
            ("installed", serde_json::json!(true)),
            ("verified", serde_json::json!(true)),
            (
                "installedPath",
                // fs-key fold: consumers reopen this value as a real file
                // path, and a backslash is a legal POSIX filename character,
                // so the fold must be Windows-only and POSIX-exact.
                serde_json::json!(cfgd_core::to_posix_fs_key(&report.installed_path)),
            ),
            (
                "verificationMode",
                serde_json::json!(report.verification_mode.as_wire_str()),
            ),
        ],
    ));

    Ok(())
}

/// Run the policy-driven self-update check at CLI startup.
///
/// Cheap by construction: it returns immediately for structured-output mode
/// (so it never pollutes the `-o json` stdout channel), and otherwise
/// interval-gates against the persisted last-checked timestamp *before* any
/// network call — a within-interval startup makes no API request. `Manual`
/// short-circuits inside [`cfgd_core::upgrade::run_update_check`].
///
/// Best-effort: any error is swallowed (logged via tracing) so a self-update
/// check never fails a normal command.
pub fn startup_update_check(printer: &Printer, config_path: &std::path::Path, assume_yes: bool) {
    use cfgd_core::config;
    use cfgd_core::upgrade::{self, UpdateCheckEffects};

    // Never interfere with machine-readable output.
    if printer.is_structured() {
        return;
    }

    let update_cfg = config::load_config(config_path)
        .ok()
        .and_then(|c| c.spec.update)
        .unwrap_or_default();

    // Cheap interval/Manual gate before constructing any effects.
    let now = cfgd_core::unix_secs_now();
    let interval = upgrade::resolved_interval(&update_cfg);
    if !upgrade::should_check(
        update_cfg.policy,
        interval,
        now,
        upgrade::last_checked_secs(),
    ) {
        return;
    }

    let interactive = std::io::IsTerminal::is_terminal(&std::io::stdin()) && !assume_yes;
    let mut effects = UpdateCheckEffects {
        interactive,
        assume_yes,
        fetch: Box::new(|channel| {
            upgrade::check_latest(env!("CARGO_PKG_VERSION"), None, channel, None)
                .map_err(unwrap_upgrade_err)
        }),
        confirm: Box::new(|c| {
            printer
                .prompt_confirm(&format!(
                    "Update available: {} {} {}. Install now?",
                    c.current,
                    printer.arrow(),
                    c.latest
                ))
                .unwrap_or(false)
        }),
        surface: Box::new(|c| {
            printer.emit(
                Doc::new()
                    .status(
                        Role::Info,
                        format!(
                            "Update available: {} {} {}",
                            c.current,
                            printer.arrow(),
                            c.latest
                        ),
                    )
                    .hint("Run 'cfgd upgrade' to install"),
            );
        }),
        apply: Box::new(|c| apply_startup_update(printer, &update_cfg, c)),
        record_checked: Box::new(|now| upgrade::record_check_at(env!("CARGO_PKG_VERSION"), now)),
    };

    let outcome = upgrade::run_update_check(&update_cfg, now, None, &mut effects);

    // Consolidated skill-stale surface. The binary surface (above) and this
    // skill surface are deduped to AT MOST ONE: `compute_update_surfaces`
    // suppresses skills whenever a binary update is pending (rule 1), and when
    // the binary is current it yields exactly ONE consolidated skill notice
    // covering both scopes (rule 3). Gated on `checked` so it rides the periodic
    // check cadence rather than firing on every command.
    if outcome.checked {
        surface_stale_skills(printer, &update_cfg, &outcome);
    }
}

/// Render the consolidated skill-stale surface for the CLI startup check.
///
/// The decision + effectful orchestration (rule 1 suppression, the
/// policy→action mapping, `Auto` refresh → re-aggregate → project-only
/// remainder) is single-sourced in [`cfgd_core::upgrade::run_standalone_skill_action`]; this
/// function only renders the returned [`cfgd_core::upgrade::StandaloneSkillOutcome`] as a
/// `Printer` Doc. It returns that outcome so tests assert the decision SHAPE,
/// not rendered text.
///
/// Only [`cfgd_core::upgrade::StandaloneSkillOutcome::NoticeNeeded`] emits — exactly one consolidated
/// notice covering both scopes. `Refreshed`/`Suppressed`/`Silent` emit nothing.
fn surface_stale_skills(
    printer: &Printer,
    update_cfg: &cfgd_core::config::UpdateConfig,
    outcome: &cfgd_core::upgrade::UpdateCheckOutcome,
) -> cfgd_core::upgrade::StandaloneSkillOutcome {
    use cfgd_core::upgrade::{self, StandaloneSkillOutcome};

    let binary_available = outcome
        .update
        .as_ref()
        .map(|u| u.update_available)
        .unwrap_or(false);
    let result = upgrade::run_standalone_skill_action(
        update_cfg,
        binary_available,
        env!("CARGO_PKG_VERSION"),
    );
    if let StandaloneSkillOutcome::NoticeNeeded(staleness) = result {
        emit_skill_stale_notice(printer, staleness);
    }
    result
}

/// Emit the single consolidated skill-stale notice, carrying the per-scope
/// counts as a structured payload for `-o json` consumers.
fn emit_skill_stale_notice(printer: &Printer, staleness: cfgd_core::upgrade::SkillStaleness) {
    use cfgd_core::upgrade::consolidated_skill_stale_message;
    printer.emit(
        Doc::new()
            .status(Role::Warn, consolidated_skill_stale_message(staleness))
            .with_data(serde_json::json!({
                "skillsStale": {
                    "user": staleness.user,
                    "project": staleness.project,
                },
            })),
    );
}

/// Extract the inner [`cfgd_core::errors::UpgradeError`] from a [`cfgd_core::errors::CfgdError`] for the startup
/// check's fetch closure, which must yield the module-level error type that
/// [`cfgd_core::upgrade::run_update_check`] threads.
fn unwrap_upgrade_err(e: cfgd_core::errors::CfgdError) -> cfgd_core::errors::UpgradeError {
    match e {
        cfgd_core::errors::CfgdError::Upgrade(u) => u,
        other => cfgd_core::errors::UpgradeError::ApiError {
            message: other.to_string(),
        },
    }
}

/// Drive the apply path for an available update during the startup check,
/// emitting the same success surface as `cfgd upgrade`. Returns whether the
/// install succeeded.
fn apply_startup_update(
    printer: &Printer,
    update_cfg: &cfgd_core::config::UpdateConfig,
    check: &cfgd_core::upgrade::UpdateCheck,
) -> bool {
    use cfgd_core::upgrade;

    let Some(release) = check.release.as_ref() else {
        return false;
    };
    let asset = match upgrade::find_asset_for_platform(release) {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!(error = %e, "startup update: no asset for platform");
            return false;
        }
    };
    match upgrade::install_release(
        release,
        asset,
        false,
        update_cfg,
        env!("CARGO_PKG_VERSION"),
        Some(printer),
    ) {
        Ok(applied) => {
            let report = &applied.report;
            printer.emit(upgraded_doc(
                &check.latest.to_string(),
                report.installed_path.display_posix(),
                applied.daemon_terminated,
                [
                    (
                        "currentVersion",
                        serde_json::json!(check.current.to_string()),
                    ),
                    ("targetVersion", serde_json::json!(check.latest.to_string())),
                    ("installed", serde_json::json!(true)),
                    (
                        "verificationMode",
                        serde_json::json!(report.verification_mode.as_wire_str()),
                    ),
                ],
            ));
            true
        }
        Err(e) => {
            tracing::warn!(error = %e, "startup update: install failed");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use cfgd_core::output::{Printer, strip_ansi};
    use cfgd_core::test_helpers::EnvVarGuard;
    use serial_test::serial;

    use super::*;

    const GITHUB_API_BASE_ENV: &str = "CFGD_GITHUB_API_BASE";

    /// Downcast a returned upgrade error to its `CliErrorMeta` so tests can pin
    /// the `error_kind` / `extras` schema the central sink now renders (the
    /// handler returns the carrier instead of emitting an error Doc).
    fn upgrade_error_meta(err: &anyhow::Error) -> &crate::cli::CliErrorMeta {
        err.downcast_ref::<crate::cli::CliErrorMeta>()
            .expect("upgrade handler returns a CliErrorMeta carrier")
    }

    /// One builder is only one builder while both install paths still call it.
    /// A helper that appended only the daemon line, with the payload key minted
    /// at each call site, let the two channels disagree; a builder minting both
    /// still lets an install path go silent by dropping its emit. This reads the
    /// module back and pins both halves: the success sentence exists once, and
    /// the production half of the file still emits it twice.
    #[test]
    fn the_success_doc_is_minted_in_exactly_one_place() {
        let source = include_str!("upgrade.rs");
        // Split so this test's own literals are not what it counts.
        let sentence = format!("cfgd upgraded {}", "to {version}");
        assert_eq!(
            source.matches(sentence.as_str()).count(),
            1,
            "both install paths must emit through upgraded_doc, not their own Doc"
        );
        let retired_key = format!("daemon{}", "Restarted");
        assert_eq!(
            source.matches(retired_key.as_str()).count(),
            0,
            "the payload key says what the code does: the daemon is terminated, \
             never restarted"
        );
        // Everything above the test module: this module's own calls must not
        // stand in for the install paths' calls.
        let production = source
            .split_once("#[cfg(test)]")
            .map(|(before, _)| before)
            .unwrap_or(source);
        assert_eq!(
            production.matches("printer.emit(upgraded_doc(").count(),
            2,
            "both install paths — cfgd upgrade and the startup policy update — \
             must still emit the upgraded doc"
        );
    }

    /// The human render and the `-o json` payload owe the reader the SAME
    /// daemon fact: a terminated daemon that no service manager owns stays down
    /// until the user starts it, so a payload saying one thing while the human
    /// line says another (or says nothing) sends half the readers the wrong way.
    /// One builder mints both, and this pins them together.
    #[test]
    fn a_terminated_daemon_is_reported_to_both_readers() {
        let (printer, cap) = Printer::for_test_doc();
        printer.emit(upgraded_doc(
            "v9.9.0",
            "/usr/local/bin/cfgd".to_string(),
            true,
            [("installed", serde_json::json!(true))],
        ));
        let human = strip_ansi(&cap.human());
        assert!(
            human.contains("Daemon") && human.contains("terminated"),
            "a terminated daemon must be reported to the human reader, got: {human:?}"
        );
        let json = cap.json().expect("the doc carries a payload");
        assert_eq!(
            json["daemonTerminated"], true,
            "the payload must say what the human line says: {json}"
        );

        let (printer, cap) = Printer::for_test_doc();
        printer.emit(upgraded_doc(
            "v9.9.0",
            "/usr/local/bin/cfgd".to_string(),
            false,
            [("installed", serde_json::json!(true))],
        ));
        let human = strip_ansi(&cap.human());
        assert!(
            !human.contains("Daemon"),
            "no daemon was running, so no daemon line is owed, got: {human:?}"
        );
        let json = cap.json().expect("the doc carries a payload");
        assert_eq!(
            json["daemonTerminated"], false,
            "the payload still states the fact when it is false: {json}"
        );
    }

    /// The payload's `installedPath` is serialized AND reopened as a real file
    /// path by consumers, so it takes the fs-key fold: Windows-only, where `\`
    /// cannot occur in a filename and the substitution is reversible. On POSIX
    /// a backslash is a legal filename byte and the value must stay exact —
    /// which also means the behavioral half of this pin only discriminates
    /// between the two folds on Windows; the winserver run is what gives it
    /// its teeth there, and the source pin is what guards the call site here.
    #[test]
    fn the_installed_path_payload_takes_the_fs_key_fold() {
        let source = include_str!("upgrade.rs");
        let production = source
            .split_once("#[cfg(test)]")
            .map(|(before, _)| before)
            .unwrap_or(source);
        // Split so this test's own literals are not what it counts.
        let unconditional = format!("to_posix_{}", "string(");
        assert_eq!(
            production.matches(unconditional.as_str()).count(),
            0,
            "installedPath names a real file consumers reopen; the unconditional \
             comparison fold renames it on POSIX where a backslash is a legal \
             filename byte"
        );
        let fs_key = format!("to_posix_{}", "fs_key(");
        assert_eq!(
            production.matches(fs_key.as_str()).count(),
            1,
            "the installedPath payload entry takes the fs-key fold"
        );

        let path = std::path::Path::new(r"C:\Program Files\cfgd\cfgd.exe");
        let (printer, cap) = Printer::for_test_doc();
        printer.emit(upgraded_doc(
            "v9.9.0",
            "C:/Program Files/cfgd/cfgd.exe".to_string(),
            false,
            [(
                "installedPath",
                serde_json::json!(cfgd_core::to_posix_fs_key(path)),
            )],
        ));
        let json = cap.json().expect("the doc carries a payload");
        if cfg!(windows) {
            assert_eq!(json["installedPath"], "C:/Program Files/cfgd/cfgd.exe");
        } else {
            assert_eq!(json["installedPath"], r"C:\Program Files\cfgd\cfgd.exe");
        }
    }

    fn current_version_tag() -> String {
        format!("v{}", env!("CARGO_PKG_VERSION"))
    }

    fn current_version_str() -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn platform_asset_name(version: &str) -> String {
        let os = std::env::consts::OS;
        let archive_os = if os == "macos" { "darwin" } else { os };
        // anodizer names archives with the Go arch (amd64/arm64), so the mock
        // must use that name or the production resolver returns no_release
        // before the download arm can fire.
        let go_arch = match std::env::consts::ARCH {
            "x86_64" => "amd64",
            "aarch64" => "arm64",
            other => other,
        };
        // Windows ships .zip; every other target ships .tar.gz.
        let suffix = if cfg!(windows) { "zip" } else { "tar.gz" };
        format!("cfgd-{}-{}-{}.{}", version, archive_os, go_arch, suffix)
    }

    /// Name of the per-artifact checksum asset (`<archive>.sha256`) anodizer
    /// publishes alongside each archive.
    fn checksum_asset_name(version: &str) -> String {
        format!("{}.sha256", platform_asset_name(version))
    }

    fn release_json_current_version() -> String {
        let tag = current_version_tag();
        format!(r#"{{"tag_name": "{tag}", "assets": []}}"#)
    }

    /// GitHub returns a 500 during `--check` → function returns Err and emits
    /// error_doc with kind "check_failed".
    #[test]
    #[serial]
    fn cmd_upgrade_check_only_api_error_500() {
        let mut server = mockito::Server::new();
        let _mock = server
            .mock("GET", "/repos/tj-smith47/cfgd/releases/latest")
            .with_status(500)
            .with_body(r#"{"message": "Internal Server Error"}"#)
            .create();
        let _guard = EnvVarGuard::set(GITHUB_API_BASE_ENV, &server.url());

        let (printer, _cap) = Printer::for_test_doc();
        let result = cmd_upgrade(
            &printer,
            std::path::Path::new("/nonexistent/cfgd.yaml"),
            true,
            false,
        );

        let err = result.expect_err("API 500 must return Err");
        let meta = upgrade_error_meta(&err);
        assert_eq!(
            meta.error_kind, "check_failed",
            "error kind must be check_failed, got: {meta:?}"
        );
        assert!(
            meta.extras["currentVersion"].is_string(),
            "currentVersion must be present in error payload: {:?}",
            meta.extras
        );
    }

    /// GitHub returns 404 during `--check` → same error path as 500.
    #[test]
    #[serial]
    fn cmd_upgrade_check_only_api_error_404() {
        let mut server = mockito::Server::new();
        let _mock = server
            .mock("GET", "/repos/tj-smith47/cfgd/releases/latest")
            .with_status(404)
            .with_body(r#"{"message": "Not Found"}"#)
            .create();
        let _guard = EnvVarGuard::set(GITHUB_API_BASE_ENV, &server.url());

        let (printer, _cap) = Printer::for_test_doc();
        let result = cmd_upgrade(
            &printer,
            std::path::Path::new("/nonexistent/cfgd.yaml"),
            true,
            false,
        );

        let err = result.expect_err("API 404 must return Err");
        assert_eq!(
            upgrade_error_meta(&err).error_kind,
            "check_failed",
            "error kind must be check_failed"
        );
    }

    /// Latest version matches current → emits "up to date" Doc, returns Ok.
    #[test]
    #[serial]
    fn cmd_upgrade_check_only_up_to_date() {
        let mut server = mockito::Server::new();
        let _mock = server
            .mock("GET", "/repos/tj-smith47/cfgd/releases/latest")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(release_json_current_version())
            .create();
        let _guard = EnvVarGuard::set(GITHUB_API_BASE_ENV, &server.url());

        let (printer, cap) = Printer::for_test_doc();
        let result = cmd_upgrade(
            &printer,
            std::path::Path::new("/nonexistent/cfgd.yaml"),
            true,
            false,
        );

        assert!(
            result.is_ok(),
            "up-to-date check must return Ok: {:?}",
            result
        );
        let json = cap.json().expect("Doc must be emitted for up-to-date case");
        assert_eq!(
            json["updateAvailable"].as_bool(),
            Some(false),
            "updateAvailable must be false: {json}"
        );
        assert_eq!(
            json["currentVersion"].as_str(),
            Some(current_version_str()),
            "currentVersion must equal the running binary version: {json}"
        );
        let human = strip_ansi(&cap.human());
        assert!(
            human.contains("up to date") || human.contains(current_version_str()),
            "human output must confirm up-to-date status, got: {human}"
        );
    }

    /// An explicit `cfgd upgrade --check` must perform the check even with
    /// every automatic-check opt-out variable set: `cmd_upgrade` never routes
    /// through `should_check`, so the opt-out (which only gates the two
    /// automatic paths) cannot suppress a user-requested check.
    #[test]
    #[serial]
    fn cmd_upgrade_check_only_ignores_optout_vars() {
        let mut server = mockito::Server::new();
        let _mock = server
            .mock("GET", "/repos/tj-smith47/cfgd/releases/latest")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(release_json_current_version())
            .create();
        let _guard = EnvVarGuard::set(GITHUB_API_BASE_ENV, &server.url());
        let _no_update_check = EnvVarGuard::set("CFGD_NO_UPDATE_CHECK", "1");
        let _no_update_notifier = EnvVarGuard::set("NO_UPDATE_NOTIFIER", "1");
        let _do_not_track = EnvVarGuard::set("DO_NOT_TRACK", "1");
        assert!(
            cfgd_core::upgrade::update_optout_var().is_some(),
            "sanity: an opt-out must actually be active for this test to prove anything"
        );

        let (printer, cap) = Printer::for_test_doc();
        let result = cmd_upgrade(
            &printer,
            std::path::Path::new("/nonexistent/cfgd.yaml"),
            true,
            false,
        );

        assert!(
            result.is_ok(),
            "explicit --check must run even with every opt-out var set: {result:?}"
        );
        let json = cap
            .json()
            .expect("Doc must be emitted; the check must have actually run");
        assert_eq!(
            json["currentVersion"].as_str(),
            Some(current_version_str()),
            "the network check must have run and reported the current version: {json}"
        );
        _mock.assert();
    }

    /// `--check` with an available update: subprocess test because cmd_upgrade calls
    /// process::exit(2) before returning. The parent spawns the test binary with a
    /// sentinel env var; the child body only executes when that var is set so the
    /// regular test runner skips it.
    ///
    /// Skipped when the test binary path is not directly executable (e.g. under
    /// cargo-llvm-cov which wraps the binary with a loader shim).
    #[test]
    #[serial]
    fn cmd_upgrade_check_only_update_available_exits_2() {
        let exe = match std::env::current_exe() {
            Ok(p) if p.exists() => p,
            _ => return,
        };
        let status = std::process::Command::new(&exe)
            .args([
                "--test-threads=1",
                "--nocapture",
                "cmd_upgrade_check_only_update_available_child",
            ])
            .env("CFGD_TEST_UPDATE_AVAILABLE_CHILD", "1")
            .status();
        let status = match status {
            Ok(s) => s,
            Err(_) => return,
        };
        assert_eq!(
            status.code(),
            Some(2),
            "upgrade --check with a newer release must exit with code 2 (UpdateAvailable)"
        );
    }

    /// Child body for `cmd_upgrade_check_only_update_available_exits_2`.
    /// Guarded by `CFGD_TEST_UPDATE_AVAILABLE_CHILD=1` so the regular test
    /// runner skips it; only the subprocess spawned by the parent activates it.
    #[test]
    #[serial]
    fn cmd_upgrade_check_only_update_available_child() {
        if std::env::var("CFGD_TEST_UPDATE_AVAILABLE_CHILD").as_deref() != Ok("1") {
            return;
        }
        let mut server = mockito::Server::new();
        let _mock = server
            .mock("GET", "/repos/tj-smith47/cfgd/releases/latest")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"tag_name": "v9.9.9", "assets": []}"#)
            .create();
        let _guard = EnvVarGuard::set(GITHUB_API_BASE_ENV, &server.url());

        let (printer, _cap) = Printer::for_test_doc();
        let _ = cmd_upgrade(
            &printer,
            std::path::Path::new("/nonexistent/cfgd.yaml"),
            true,
            false,
        );
    }

    /// GitHub returns 500 during the full upgrade flow → returns Err and emits
    /// error_doc with kind "check_failed".
    #[test]
    #[serial]
    fn cmd_upgrade_full_check_api_error() {
        let mut server = mockito::Server::new();
        let _mock = server
            .mock("GET", "/repos/tj-smith47/cfgd/releases/latest")
            .with_status(500)
            .create();
        let _guard = EnvVarGuard::set(GITHUB_API_BASE_ENV, &server.url());

        let (printer, _cap) = Printer::for_test_doc();
        let result = cmd_upgrade(
            &printer,
            std::path::Path::new("/nonexistent/cfgd.yaml"),
            false,
            false,
        );

        let err = result.expect_err("API 500 during full upgrade must return Err");
        assert_eq!(
            upgrade_error_meta(&err).error_kind,
            "check_failed",
            "error kind must be check_failed on API failure"
        );
    }

    /// Latest version == current → skips download, emits "already latest", returns Ok.
    #[test]
    #[serial]
    fn cmd_upgrade_full_already_latest() {
        let mut server = mockito::Server::new();
        let _mock = server
            .mock("GET", "/repos/tj-smith47/cfgd/releases/latest")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(release_json_current_version())
            .create();
        let _guard = EnvVarGuard::set(GITHUB_API_BASE_ENV, &server.url());

        let (printer, cap) = Printer::for_test_doc();
        let result = cmd_upgrade(
            &printer,
            std::path::Path::new("/nonexistent/cfgd.yaml"),
            false,
            false,
        );

        assert!(
            result.is_ok(),
            "already-latest must return Ok: {:?}",
            result
        );
        let json = cap
            .json()
            .expect("Doc must be emitted for already-latest case");
        assert_eq!(
            json["updateAvailable"].as_bool(),
            Some(false),
            "updateAvailable must be false: {json}"
        );
        assert_eq!(
            json["downloaded"].as_bool(),
            Some(false),
            "downloaded must be false when already at latest: {json}"
        );
        assert_eq!(
            json["installed"].as_bool(),
            Some(false),
            "installed must be false when already at latest: {json}"
        );
    }

    /// Release has no asset matching the current platform → error_doc with
    /// kind "no_release" is emitted and Err is returned.
    #[test]
    #[serial]
    fn cmd_upgrade_full_no_platform_asset() {
        let mut server = mockito::Server::new();
        let _mock = server
            .mock("GET", "/repos/tj-smith47/cfgd/releases/latest")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                    "tag_name": "v9.9.9",
                    "assets": [
                        {
                            "name": "cfgd-9.9.9-fakeos-fakearch.tar.gz",
                            "browser_download_url": "http://example.com/fake.tar.gz",
                            "size": 1024
                        }
                    ]
                }"#,
            )
            .create();
        let _guard = EnvVarGuard::set(GITHUB_API_BASE_ENV, &server.url());

        let (printer, _cap) = Printer::for_test_doc();
        let result = cmd_upgrade(
            &printer,
            std::path::Path::new("/nonexistent/cfgd.yaml"),
            false,
            false,
        );

        let err = result.expect_err("missing platform asset must return Err");
        assert_eq!(
            upgrade_error_meta(&err).error_kind,
            "no_release",
            "error kind must be no_release"
        );
    }

    /// Release has the platform asset, but the asset download URL returns 500 →
    /// error_doc with kind "install_failed" is emitted and Err is returned.
    #[test]
    #[serial]
    fn cmd_upgrade_full_download_failure() {
        let version = "9.9.9";
        let asset_name = platform_asset_name(version);
        let checksum_name = checksum_asset_name(version);

        let mut server = mockito::Server::new();

        let release_body = format!(
            r#"{{
                "tag_name": "v{version}",
                "assets": [
                    {{
                        "name": "{asset_name}",
                        "browser_download_url": "{url}/{asset_name}",
                        "size": 1048576
                    }},
                    {{
                        "name": "{checksum_name}",
                        "browser_download_url": "{url}/{checksum_name}",
                        "size": 64
                    }}
                ]
            }}"#,
            url = server.url()
        );

        let _release_mock = server
            .mock("GET", "/repos/tj-smith47/cfgd/releases/latest")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(release_body)
            .create();

        let asset_path = format!("/{asset_name}");
        let _asset_mock = server
            .mock("GET", asset_path.as_str())
            .with_status(500)
            .create();

        let _guard = EnvVarGuard::set(GITHUB_API_BASE_ENV, &server.url());
        let home = tempfile::tempdir().unwrap();
        let _home_guard = cfgd_core::with_test_home_guard(home.path());

        let (printer, _cap) = Printer::for_test_doc();
        let result = cmd_upgrade(
            &printer,
            std::path::Path::new("/nonexistent/cfgd.yaml"),
            false,
            false,
        );

        let err = result.expect_err("asset download 500 must return Err");
        let meta = upgrade_error_meta(&err);
        assert_eq!(
            meta.error_kind, "install_failed",
            "error kind must be install_failed"
        );
        assert_eq!(
            meta.extras["currentVersion"].as_str(),
            Some(current_version_str()),
            "currentVersion must be present in error payload: {:?}",
            meta.extras
        );
    }

    /// Strict cosign mode (`--require-cosign` / `CFGD_REQUIRE_COSIGN=1`):
    /// release ships the archive + its per-artifact `<archive>.sha256` but no
    /// keyless cosign bundle (`<archive>.sha256.cosign.bundle`). The CLI must
    /// surface the failure with kind `cosign_required` (distinct from
    /// `install_failed`) and carry `requireCosign: true` in the error payload
    /// so alerting can route strict-mode failures separately from generic
    /// install errors. Pins end-to-end thread-through of the flag from the
    /// clap surface into the error_doc.
    ///
    /// The archive bytes are arbitrary — strict cosign fires inside
    /// `download_and_install_to` (in `verify_cosign_bundle`) BEFORE the
    /// checksum comparison and extract, so the `.sha256` body need only be a
    /// well-formed bare hash for the asset to resolve and download. The
    /// missing bundle short-circuits with `CosignRequired` first. Avoids
    /// pulling in `flate2` + `tar` as dev-dependencies of the binary crate
    /// just to assemble a real tarball.
    #[test]
    #[serial]
    fn cmd_upgrade_strict_cosign_fails_when_release_has_no_bundle() {
        use cfgd_core::test_helpers::CosignTestShim;

        let version = "9.9.9";
        let asset_name = platform_asset_name(version);
        let checksum_name = checksum_asset_name(version);
        let archive_body: &[u8] = b"placeholder archive bytes";
        // Per-artifact checksum holds the bare SHA256 of the archive bytes.
        let checksum_body = cfgd_core::sha256_hex(archive_body);

        let _shim = CosignTestShim::builder().with_argv_logging(false).install();

        let mut server = mockito::Server::new();
        let release_body = format!(
            r#"{{
                "tag_name": "v{version}",
                "assets": [
                    {{
                        "name": "{asset_name}",
                        "browser_download_url": "{url}/{asset_name}",
                        "size": {archive_size}
                    }},
                    {{
                        "name": "{checksum_name}",
                        "browser_download_url": "{url}/{checksum_name}",
                        "size": {checksum_size}
                    }}
                ]
            }}"#,
            url = server.url(),
            archive_size = archive_body.len(),
            checksum_size = checksum_body.len()
        );
        let _release_mock = server
            .mock("GET", "/repos/tj-smith47/cfgd/releases/latest")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(release_body)
            .create();
        let _archive_mock = server
            .mock("GET", format!("/{asset_name}").as_str())
            .with_status(200)
            .with_body(archive_body)
            .create();
        // The release deliberately omits `<archive>.sha256.cosign.bundle`;
        // only the archive and its `.sha256` are served.
        let _checksum_mock = server
            .mock("GET", format!("/{checksum_name}").as_str())
            .with_status(200)
            .with_body(&checksum_body)
            .create();

        let _guard = EnvVarGuard::set(GITHUB_API_BASE_ENV, &server.url());
        let home = tempfile::tempdir().unwrap();
        let _home_guard = cfgd_core::with_test_home_guard(home.path());

        let (printer, _cap) = Printer::for_test_doc();
        let result = cmd_upgrade(
            &printer,
            std::path::Path::new("/nonexistent/cfgd.yaml"),
            false,
            true,
        );

        let err = result.expect_err("strict cosign + missing bundle must return Err");
        let meta = upgrade_error_meta(&err);
        assert_eq!(
            meta.error_kind, "cosign_required",
            "error kind must be distinct from install_failed so alerting can route strict-mode failures"
        );
        assert_eq!(
            meta.extras["requireCosign"].as_bool(),
            Some(true),
            "error payload must carry requireCosign=true for downstream consumers: {:?}",
            meta.extras
        );
    }

    // ----- Wired skill surface (rule 1 / rule 3 / Auto refresh) -----
    // Rule numbering per `cfgd_core::upgrade::dedup`'s module docs.

    use cfgd_core::config::{SkillUpdateConfig, SkillUpdatePolicy, UpdateConfig, UpdatePolicy};
    use cfgd_core::generate::{SkillKind, skill_model_for};
    use cfgd_core::providers::skill::{ClaudeCodeProvider, SkillProvider, SkillScope};
    use cfgd_core::test_helpers::CwdGuard;
    use cfgd_core::upgrade::{
        SkillStaleness, StandaloneSkillOutcome, UpdateCheck, UpdateCheckOutcome,
    };
    use cfgd_core::with_test_home;

    fn update_cfg(policy: UpdatePolicy, skills: SkillUpdatePolicy) -> UpdateConfig {
        UpdateConfig {
            policy,
            interval: "24h".to_string(),
            channel: None,
            skills: SkillUpdateConfig { policy: skills },
        }
    }

    /// An [`UpdateCheckOutcome`] reporting a check that ran, with `available`
    /// flagging whether a newer binary is pending.
    fn outcome(available: bool) -> UpdateCheckOutcome {
        let current = semver::Version::new(1, 0, 0);
        let latest = if available {
            semver::Version::new(2, 0, 0)
        } else {
            current.clone()
        };
        UpdateCheckOutcome {
            checked: true,
            surfaced: false,
            applied: false,
            update: Some(UpdateCheck {
                current,
                latest,
                update_available: available,
                release: None,
            }),
        }
    }

    /// Install a claude-code skill at `scope`, then stale its version stamp.
    fn seed_stale(kind: SkillKind, scope: SkillScope) -> std::path::PathBuf {
        let path = ClaudeCodeProvider
            .install(&skill_model_for(kind, env!("CARGO_PKG_VERSION")), scope)
            .expect("install skill");
        let body = std::fs::read_to_string(&path).expect("read skill");
        let staled = body
            .lines()
            .map(|l| {
                if l.trim_start().starts_with("cfgd-version:") {
                    "cfgd-version: 0.0.1".to_string()
                } else {
                    l.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&path, staled).expect("write staled skill");
        path
    }

    #[test]
    #[serial]
    fn wired_notify_standalone_stale_emits_one_consolidated_notice() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let _rt = EnvVarGuard::set("CFGD_RUNTIME_DIR", &runtime.path().to_string_lossy());
        let _cwd = CwdGuard::set(project.path()).unwrap();

        with_test_home(home.path(), || {
            seed_stale(SkillKind::Module, SkillScope::User);
            seed_stale(SkillKind::Module, SkillScope::Project);

            let (printer, _cap) = Printer::for_test_doc();
            let cfg = update_cfg(UpdatePolicy::Notify, SkillUpdatePolicy::Inherit);
            let surface = surface_stale_skills(&printer, &cfg, &outcome(false));

            // Exactly ONE consolidated notice covering both scopes (user:1,project:1).
            assert_eq!(
                surface,
                StandaloneSkillOutcome::NoticeNeeded(SkillStaleness {
                    user: 1,
                    project: 1
                }),
                "binary current + both scopes stale → one consolidated notice"
            );
        });
    }

    #[test]
    #[serial]
    fn wired_binary_pending_suppresses_skill_surface() {
        let home = tempfile::tempdir().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let _rt = EnvVarGuard::set("CFGD_RUNTIME_DIR", &runtime.path().to_string_lossy());

        with_test_home(home.path(), || {
            seed_stale(SkillKind::Module, SkillScope::User);

            let (printer, cap) = Printer::for_test_doc();
            let cfg = update_cfg(UpdatePolicy::Notify, SkillUpdatePolicy::Inherit);
            // Rule 1: a binary update is pending → skill surface suppressed.
            let surface = surface_stale_skills(&printer, &cfg, &outcome(true));

            assert_eq!(
                surface,
                StandaloneSkillOutcome::Suppressed,
                "rule 1 suppresses skills"
            );
            assert!(
                strip_ansi(&cap.human()).is_empty(),
                "no skill notice emitted when binary is pending"
            );
        });
    }

    #[test]
    #[serial]
    fn wired_auto_standalone_refreshes_user_leaves_project() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let _rt = EnvVarGuard::set("CFGD_RUNTIME_DIR", &runtime.path().to_string_lossy());
        let _cwd = CwdGuard::set(project.path()).unwrap();

        with_test_home(home.path(), || {
            seed_stale(SkillKind::Module, SkillScope::User);
            let project_path = seed_stale(SkillKind::Module, SkillScope::Project);
            let project_before = std::fs::read(&project_path).unwrap();

            let (printer, _cap) = Printer::for_test_doc();
            let cfg = update_cfg(UpdatePolicy::Auto, SkillUpdatePolicy::Inherit);
            let surface = surface_stale_skills(&printer, &cfg, &outcome(false));

            // User-scope refreshed in place; project-scope still stale → the
            // single remaining notice covers project only (user:0, project:1).
            assert_eq!(
                surface,
                StandaloneSkillOutcome::NoticeNeeded(SkillStaleness {
                    user: 0,
                    project: 1
                }),
                "Auto refreshes user-scope, leaves a project-only notice"
            );
            // Project bytes byte-identical — never auto-written.
            let project_after = std::fs::read(&project_path).unwrap();
            assert_eq!(
                project_before, project_after,
                "Auto must not rewrite the tracked project skill"
            );
        });
    }

    #[test]
    #[serial]
    fn wired_manual_standalone_stale_is_silent() {
        let home = tempfile::tempdir().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let _rt = EnvVarGuard::set("CFGD_RUNTIME_DIR", &runtime.path().to_string_lossy());

        with_test_home(home.path(), || {
            seed_stale(SkillKind::Module, SkillScope::User);

            let (printer, cap) = Printer::for_test_doc();
            let cfg = update_cfg(UpdatePolicy::Manual, SkillUpdatePolicy::Inherit);
            let surface = surface_stale_skills(&printer, &cfg, &outcome(false));

            assert_eq!(
                surface,
                StandaloneSkillOutcome::Silent,
                "Manual standalone-stale is silent"
            );
            assert!(
                strip_ansi(&cap.human()).is_empty(),
                "Manual emits no skill notice"
            );
        });
    }
}
