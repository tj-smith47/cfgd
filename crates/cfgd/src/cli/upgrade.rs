use super::*;
use cfgd_core::output_v2::{Doc, Printer as PrinterV2, Role};

pub fn cmd_upgrade(
    printer: &Printer,
    v2_printer: &PrinterV2,
    check_only: bool,
) -> anyhow::Result<()> {
    use cfgd_core::upgrade;

    if check_only {
        let check = upgrade::check_latest(None, Some(printer)).map_err(|e| {
            v2_printer.emit(cfgd_core::output_v2::error_doc(
                env!("CARGO_PKG_VERSION"),
                "check_failed",
                format!("Failed to check latest version: {e}"),
                serde_json::json!({ "currentVersion": env!("CARGO_PKG_VERSION") }),
            ));
            e
        })?;

        if check.update_available {
            v2_printer.emit(
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
            v2_printer.emit(
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

    v2_printer.heading("Upgrade");

    let check = upgrade::check_latest(None, Some(printer)).map_err(|e| {
        v2_printer.emit(cfgd_core::output_v2::error_doc(
            env!("CARGO_PKG_VERSION"),
            "check_failed",
            format!("Failed to check latest version: {e}"),
            serde_json::json!({ "currentVersion": env!("CARGO_PKG_VERSION") }),
        ));
        e
    })?;

    if !check.update_available {
        v2_printer.emit(
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
        v2_printer.emit(cfgd_core::output_v2::error_doc(
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
        v2_printer.emit(cfgd_core::output_v2::error_doc(
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
        let sec = v2_printer.section(format!(
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
            v2_printer.emit(cfgd_core::output_v2::error_doc(
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

    v2_printer.emit(
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
    use super::*;

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
        // 1536 bytes = 1.5 KB
        assert_eq!(format_bytes(1536), "1.5 KB");
    }

    #[test]
    fn format_bytes_just_below_mb_boundary() {
        // 1024*1024 - 1 = 1048575
        assert_eq!(format_bytes(1048575), "1024.0 KB");
    }

    #[test]
    fn format_bytes_exact_mb_boundary() {
        assert_eq!(format_bytes(1024 * 1024), "1.0 MB");
    }

    #[test]
    fn format_bytes_large_mb_value() {
        // 52428800 = 50 MB
        assert_eq!(format_bytes(52_428_800), "50.0 MB");
    }

    #[test]
    fn format_bytes_fractional_mb() {
        // 1.5 MB = 1572864
        assert_eq!(format_bytes(1_572_864), "1.5 MB");
    }
}
