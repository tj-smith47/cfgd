use cfgd_core::PathDisplayExt;
use cfgd_core::output::{Doc, Printer, Role};

pub fn cmd_upgrade(
    printer: &Printer,
    check_only: bool,
    require_cosign: bool,
) -> anyhow::Result<()> {
    use cfgd_core::upgrade;

    if check_only {
        let check = upgrade::check_latest(None, Some(printer)).map_err(|e| {
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
                        format!("Update available: {} -> {}", check.current, check.latest),
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

    let check = upgrade::check_latest(None, Some(printer)).map_err(|e| {
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
        let sec = printer.section(format!(
            "Update available: {} -> {}",
            check.current, check.latest
        ));
        sec.kv("Binary", &asset.name);
        if asset.size > 0 {
            sec.kv("Size", format_bytes(asset.size));
        }
    }

    let report = upgrade::download_and_install(release, asset, require_cosign, Some(printer))
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

    // Invalidate version cache since we just upgraded
    upgrade::invalidate_cache();

    // Restart daemon if running
    let daemon_restarted = upgrade::restart_daemon_if_running();

    printer.emit(
        Doc::new()
            .status(Role::Ok, format!("cfgd upgraded to {}", check.latest))
            .kv("Installed to", report.installed_path.display_posix())
            .with_data(serde_json::json!({
                "currentVersion": check.current.to_string(),
                "targetVersion": check.latest.to_string(),
                "downloaded": true,
                "installed": true,
                "verified": true,
                "daemonRestarted": daemon_restarted,
                "installedPath": report.installed_path.display().to_string(),
                "verificationMode": report.verification_mode.as_wire_str(),
            })),
    );

    Ok(())
}

/// Run the policy-driven self-update check at CLI startup.
///
/// Cheap by construction: it returns immediately for structured-output mode
/// (so it never pollutes the `-o json` stdout channel), and otherwise
/// interval-gates against the persisted last-checked timestamp *before* any
/// network call — a within-interval startup makes no API request. `Manual`
/// short-circuits inside [`run_update_check`].
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
        fetch: Box::new(|_channel| upgrade::check_latest(None, None).map_err(unwrap_upgrade_err)),
        confirm: Box::new(|c| {
            printer
                .prompt_confirm(&format!(
                    "Update available: {} -> {}. Install now?",
                    c.current, c.latest
                ))
                .unwrap_or(false)
        }),
        surface: Box::new(|c| {
            printer.emit(
                Doc::new()
                    .status(
                        Role::Info,
                        format!("Update available: {} -> {}", c.current, c.latest),
                    )
                    .hint("Run 'cfgd upgrade' to install"),
            );
        }),
        apply: Box::new(|c| apply_startup_update(printer, c)),
        record_checked: Box::new(upgrade::record_check_at),
    };

    let _ = upgrade::run_update_check(&update_cfg, now, None, &mut effects);
}

/// Extract the inner [`UpgradeError`] from a [`CfgdError`] for the startup
/// check's fetch closure, which must yield the module-level error type that
/// [`run_update_check`] threads.
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
fn apply_startup_update(printer: &Printer, check: &cfgd_core::upgrade::UpdateCheck) -> bool {
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
    match upgrade::download_and_install(release, asset, false, Some(printer)) {
        Ok(report) => {
            upgrade::invalidate_cache();
            let daemon_restarted = upgrade::restart_daemon_if_running();
            printer.emit(
                Doc::new()
                    .status(Role::Ok, format!("cfgd upgraded to {}", check.latest))
                    .kv("Installed to", report.installed_path.display_posix())
                    .with_data(serde_json::json!({
                        "currentVersion": check.current.to_string(),
                        "targetVersion": check.latest.to_string(),
                        "installed": true,
                        "daemonRestarted": daemon_restarted,
                        "verificationMode": report.verification_mode.as_wire_str(),
                    })),
            );
            true
        }
        Err(e) => {
            tracing::warn!(error = %e, "startup update: install failed");
            false
        }
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{} B", bytes)
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

    #[test]
    fn format_bytes_zero() {
        assert_eq!(format_bytes(0), "0 B");
    }

    #[test]
    fn format_bytes_small_value() {
        assert_eq!(format_bytes(512), "512 B");
    }

    #[test]
    fn format_bytes_just_below_kb_boundary() {
        assert_eq!(format_bytes(1023), "1023 B");
    }

    #[test]
    fn format_bytes_exact_kb_boundary() {
        assert_eq!(format_bytes(1024), "1.0 KB");
    }

    #[test]
    fn format_bytes_fractional_kb() {
        assert_eq!(format_bytes(1536), "1.5 KB");
    }

    #[test]
    fn format_bytes_just_below_mb_boundary() {
        assert_eq!(format_bytes(1048575), "1024.0 KB");
    }

    #[test]
    fn format_bytes_exact_mb_boundary() {
        assert_eq!(format_bytes(1024 * 1024), "1.0 MB");
    }

    #[test]
    fn format_bytes_large_mb_value() {
        assert_eq!(format_bytes(52_428_800), "50.0 MB");
    }

    #[test]
    fn format_bytes_fractional_mb() {
        assert_eq!(format_bytes(1_572_864), "1.5 MB");
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
        let result = cmd_upgrade(&printer, true, false);

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
        let result = cmd_upgrade(&printer, true, false);

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
        let result = cmd_upgrade(&printer, true, false);

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
        let _ = cmd_upgrade(&printer, true, false);
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
        let result = cmd_upgrade(&printer, false, false);

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
        let result = cmd_upgrade(&printer, false, false);

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
        let result = cmd_upgrade(&printer, false, false);

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
        let result = cmd_upgrade(&printer, false, false);

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
    #[cfg(unix)]
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
        let result = cmd_upgrade(&printer, false, true);

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
}
