use super::*;

pub(super) fn cmd_upgrade(printer: &Printer, check_only: bool) -> anyhow::Result<()> {
    use cfgd_core::upgrade;

    if check_only {
        let check = upgrade::check_latest(None, Some(printer))?;

        if check.update_available {
            printer.info(&format!(
                "Update available: {} -> {}",
                check.current, check.latest
            ));
            printer.info("Run 'cfgd upgrade' to install");
            // "Action needed, not an error" — reserves Error (1) for
            // actual failures so scripts can distinguish `--check`
            // results from network/IO errors.
            cfgd_core::exit::ExitCode::UpdateAvailable.exit();
        } else {
            printer.success(&format!("cfgd {} is up to date", check.current));
        }

        return Ok(());
    }

    printer.header("Upgrade");

    let check = upgrade::check_latest(None, Some(printer))?;

    if !check.update_available {
        printer.success(&format!(
            "cfgd {} is already the latest version",
            check.current
        ));
        return Ok(());
    }

    printer.info(&format!(
        "Update available: {} -> {}",
        check.current, check.latest
    ));

    let release = check
        .release
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("release info not available"))?;

    let asset = upgrade::find_asset_for_platform(release)?;
    printer.key_value("Binary", &asset.name);
    if asset.size > 0 {
        printer.key_value("Size", &format_bytes(asset.size));
    }
    printer.newline();

    let installed_path = upgrade::download_and_install(release, asset, Some(printer))?;
    printer.success(&format!("Installed to {}", installed_path.display()));

    // Invalidate version cache since we just upgraded
    upgrade::invalidate_cache();

    // Restart daemon if running
    if upgrade::restart_daemon_if_running() {
        printer.info("Daemon restarted with new version");
    }

    printer.newline();
    printer.success(&format!("cfgd upgraded to {}", check.latest));

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
