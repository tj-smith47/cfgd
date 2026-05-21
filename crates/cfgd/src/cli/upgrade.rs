use cfgd_core::output::{Doc, Printer, Role};

pub fn cmd_upgrade(printer: &Printer, check_only: bool) -> anyhow::Result<()> {
    use cfgd_core::upgrade;

    if check_only {
        let check = upgrade::check_latest(None, Some(printer)).map_err(|e| {
            printer.emit(cfgd_core::output::error_doc(
                env!("CARGO_PKG_VERSION"),
                "check_failed",
                format!("Failed to check latest version: {e}"),
                serde_json::json!({ "currentVersion": env!("CARGO_PKG_VERSION") }),
            ));
            e
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
        printer.emit(cfgd_core::output::error_doc(
            env!("CARGO_PKG_VERSION"),
            "check_failed",
            format!("Failed to check latest version: {e}"),
            serde_json::json!({ "currentVersion": env!("CARGO_PKG_VERSION") }),
        ));
        e
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
                })),
        );
        return Ok(());
    }

    let release = check.release.as_ref().ok_or_else(|| {
        printer.emit(cfgd_core::output::error_doc(
            &check.latest.to_string(),
            "no_release",
            "release info not available".to_string(),
            serde_json::json!({
                "currentVersion": check.current.to_string(),
                "latestVersion": check.latest.to_string(),
            }),
        ));
        anyhow::anyhow!("release info not available")
    })?;

    let asset = upgrade::find_asset_for_platform(release).map_err(|e| {
        printer.emit(cfgd_core::output::error_doc(
            &check.latest.to_string(),
            "no_release",
            format!("no asset for platform: {e}"),
            serde_json::json!({
                "currentVersion": check.current.to_string(),
                "latestVersion": check.latest.to_string(),
            }),
        ));
        e
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

    let installed_path =
        upgrade::download_and_install(release, asset, Some(printer)).map_err(|e| {
            printer.emit(cfgd_core::output::error_doc(
                &check.latest.to_string(),
                "install_failed",
                format!("download/install failed: {e}"),
                serde_json::json!({
                    "currentVersion": check.current.to_string(),
                    "latestVersion": check.latest.to_string(),
                }),
            ));
            e
        })?;

    // Invalidate version cache since we just upgraded
    upgrade::invalidate_cache();

    // Restart daemon if running
    let daemon_restarted = upgrade::restart_daemon_if_running();

    printer.emit(
        Doc::new()
            .status(Role::Ok, format!("cfgd upgraded to {}", check.latest))
            .kv("Installed to", installed_path.display().to_string())
            .with_data(serde_json::json!({
                "currentVersion": check.current.to_string(),
                "targetVersion": check.latest.to_string(),
                "downloaded": true,
                "installed": true,
                "verified": true,
                "daemonRestarted": daemon_restarted,
                "installedPath": installed_path.display().to_string(),
            })),
    );

    Ok(())
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

    fn current_version_tag() -> String {
        format!("v{}", env!("CARGO_PKG_VERSION"))
    }

    fn current_version_str() -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn platform_asset_name(version: &str) -> String {
        let os = std::env::consts::OS;
        let arch = std::env::consts::ARCH;
        let archive_os = if os == "macos" { "darwin" } else { os };
        format!("cfgd-{}-{}-{}.tar.gz", version, archive_os, arch)
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

    // --- cmd_upgrade: check_only=true path ---

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

        let (printer, cap) = Printer::for_test_doc();
        let result = cmd_upgrade(&printer, true);

        assert!(result.is_err(), "API 500 must return Err");
        let json = cap
            .json()
            .expect("error_doc must be emitted on API failure");
        assert_eq!(
            json["error"].as_str(),
            Some("check_failed"),
            "error kind must be check_failed, got: {json}"
        );
        assert!(
            json["currentVersion"].is_string(),
            "currentVersion must be present in error payload: {json}"
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

        let (printer, cap) = Printer::for_test_doc();
        let result = cmd_upgrade(&printer, true);

        assert!(result.is_err(), "API 404 must return Err");
        let json = cap.json().expect("error_doc must be emitted");
        assert_eq!(
            json["error"].as_str(),
            Some("check_failed"),
            "error kind must be check_failed: {json}"
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
        let result = cmd_upgrade(&printer, true);

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
        let _ = cmd_upgrade(&printer, true);
    }

    // --- cmd_upgrade: check_only=false path ---

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

        let (printer, cap) = Printer::for_test_doc();
        let result = cmd_upgrade(&printer, false);

        assert!(
            result.is_err(),
            "API 500 during full upgrade must return Err"
        );
        let json = cap.json().expect("error_doc must be emitted");
        assert_eq!(
            json["error"].as_str(),
            Some("check_failed"),
            "error kind must be check_failed on API failure: {json}"
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
        let result = cmd_upgrade(&printer, false);

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

        let (printer, cap) = Printer::for_test_doc();
        let result = cmd_upgrade(&printer, false);

        assert!(
            result.is_err(),
            "missing platform asset must return Err: {result:?}"
        );
        let json = cap
            .json()
            .expect("error_doc must be emitted for missing platform asset");
        assert_eq!(
            json["error"].as_str(),
            Some("no_release"),
            "error kind must be no_release: {json}"
        );
    }

    /// Release has the platform asset, but the asset download URL returns 500 →
    /// error_doc with kind "install_failed" is emitted and Err is returned.
    #[test]
    #[serial]
    fn cmd_upgrade_full_download_failure() {
        let version = "9.9.9";
        let asset_name = platform_asset_name(version);
        let checksums_name = format!("cfgd-{}-checksums.txt", version);

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
                        "name": "{checksums_name}",
                        "browser_download_url": "{url}/{checksums_name}",
                        "size": 256
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

        let (printer, cap) = Printer::for_test_doc();
        let result = cmd_upgrade(&printer, false);

        assert!(
            result.is_err(),
            "asset download 500 must return Err: {result:?}"
        );
        let json = cap
            .json()
            .expect("error_doc must be emitted on download failure");
        assert_eq!(
            json["error"].as_str(),
            Some("install_failed"),
            "error kind must be install_failed: {json}"
        );
        assert_eq!(
            json["currentVersion"].as_str(),
            Some(current_version_str()),
            "currentVersion must be present in error payload: {json}"
        );
    }
}
