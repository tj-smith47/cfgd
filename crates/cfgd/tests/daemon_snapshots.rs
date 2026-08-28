//! Snapshot tests for `cfgd daemon` user-facing commands.
//!
//! Cases:
//!   - `daemon_status/not_running.{txt,json}` — real `cmd_daemon_status` against
//!     a CI host with no daemon running (IPC returns `Ok(None)`).
//!   - `daemon_status/running.{txt,json}` — hand-rolled via
//!     `build_daemon_status_doc(Some(&sample))`. Standing up the daemon IPC
//!     server from a snapshot test is intractable; the Doc seam covers the
//!     running-shape branch deterministically.
//!   - `daemon_status/running_no_timestamps.txt` — hand-rolled; covers the
//!     `last_reconcile=None && last_sync=None && update_available=None &&
//!     reconcile_interval_secs=None && sync_interval_secs=None` branch.
//!   - `daemon_status/running_with_update.txt` — hand-rolled; covers the
//!     update-available banner.
//!   - `daemon_install/installed_{linux,macos,windows}.{txt,json}` — hand-rolled
//!     via `build_daemon_install_doc(&payload)`. `install_service` writes to
//!     systemd/launchctl/sc.exe targets the test host can't usefully intercept,
//!     so the Doc payload is constructed with the literal platform field; the
//!     Doc itself carries the heading + platform-specific status messages +
//!     `with_data(&payload)`, identical to what `cmd_daemon_install` emits.
//!   - `daemon_install/install_failed.{txt,json}` — hand-rolled via
//!     `error_doc("cfgd", "install_failed", ...)`.
//!   - `daemon_uninstall/uninstalled_{linux,macos,windows}.{txt,json}` —
//!     hand-rolled via `build_daemon_uninstall_doc(&payload)`.
//!   - `daemon_uninstall/uninstall_failed.{txt,json}` — hand-rolled via
//!     `error_doc("cfgd", "uninstall_failed", ...)`.
//!
//! `cfgd daemon` (foreground `Run`) and `cfgd daemon service` have no
//! snapshots from this integration test. The reconcile loop is a
//! never-returning happy path from the CLI boundary; the Windows service
//! entry point is a background process with no user-facing emit. The
//! foreground loop has no golden at all: it reports through the journal, and
//! `the_reconcile_loop_reports_through_the_journal_and_never_the_printer`
//! (`crates/cfgd-core/src/daemon/tests.rs`) holds both halves of that — a
//! silent printer and the run's account in the log.
//!
//! Goldens live under `tests/output_snapshots/daemon_{status,install,uninstall}/`.
//! Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test daemon_snapshots

use std::path::Path;

use clap::Parser;

use cfgd::cli::Cli;
use cfgd::cli::daemon::{
    DaemonInstallOutput, DaemonUninstallOutput, build_daemon_install_doc, build_daemon_status_doc,
    build_daemon_uninstall_doc, cmd_daemon_status,
};
use cfgd_core::daemon::{DaemonStatusResponse, SourceStatus};
use cfgd_core::output::Printer;

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

fn sample_status_basic() -> DaemonStatusResponse {
    DaemonStatusResponse {
        modules: vec!["base".to_string(), "dev-tools".to_string()],
        running: true,
        pid: 4242,
        uptime_secs: 3600,
        last_reconcile: Some("2026-05-12T10:00:00Z".to_string()),
        last_sync: Some("2026-05-12T09:55:00Z".to_string()),
        drift_count: 7,
        // Stored tokens, from the constants: `syncing` is a spelling nothing
        // writes into this field, so a row seeded with it pinned only what an
        // unrecognised token renders as.
        sources: vec![
            SourceStatus {
                name: "local".to_string(),
                last_sync: None,
                drift_count: None,
                status: cfgd_core::state::SOURCE_STATUS_ACTIVE.to_string(),
                last_commit: None,
            },
            SourceStatus {
                name: "team".to_string(),
                last_sync: Some("2026-05-12T09:00:00Z".to_string()),
                drift_count: None,
                status: cfgd_core::state::SOURCE_STATUS_ERROR.to_string(),
                last_commit: None,
            },
        ],
        update_available: None,
        module_reconcile: vec![],
        reconcile_interval_secs: Some(300),
        sync_interval_secs: Some(900),
        config_path: Some("/home/u/.config/cfgd/cfgd.yaml".to_string()),
        profile: Some("work".to_string()),
    }
}

fn sample_status_with_update() -> DaemonStatusResponse {
    let mut s = sample_status_basic();
    s.update_available = Some("9.9.9".to_string());
    s
}

fn sample_status_no_timestamps() -> DaemonStatusResponse {
    DaemonStatusResponse {
        modules: vec![],
        running: true,
        pid: 1,
        uptime_secs: 1,
        last_reconcile: None,
        last_sync: None,
        drift_count: 0,
        sources: vec![],
        update_available: None,
        module_reconcile: vec![],
        reconcile_interval_secs: None,
        sync_interval_secs: None,
        config_path: None,
        profile: None,
    }
}

/// The declared catalog the daemon's live rows are merged over. Without it the
/// shared `Sources` table can only render `-` in the columns the daemon never
/// reports (origin, priority, commit, signing), which is a fact about the
/// fixture rather than about the render.
fn declared_sources() -> Vec<cfgd::cli::output_types::SourceListEntry> {
    vec![
        cfgd::cli::output_types::SourceListEntry {
            name: "local".to_string(),
            url: Some("/etc/cfgd/local".to_string()),
            priority: Some(50),
            version: None,
            status: cfgd_core::state::SOURCE_STATUS_ACTIVE.to_string(),
            last_fetched: None,
            signed: None,
            require_signed_commits: Some(false),
            last_commit: None,
            drift_count: None,
        },
        cfgd::cli::output_types::SourceListEntry {
            name: "team".to_string(),
            url: Some("https://github.com/team/config".to_string()),
            priority: Some(100),
            version: Some("3.1.0".to_string()),
            status: cfgd_core::state::SOURCE_STATUS_ACTIVE.to_string(),
            last_fetched: Some("2026-05-12T09:00:00Z".to_string()),
            signed: Some(true),
            require_signed_commits: Some(true),
            last_commit: Some("abc1234567890def".to_string()),
            drift_count: None,
        },
    ]
}

// --- cfgd daemon status ----------------------------------------------------

/// The instant every daemon-status render in this suite ages its stamps
/// against, so a captured age is a fact about the fixture rather than about
/// the day the suite ran.
const DAEMON_STATUS_NOW: &str = "2026-05-14T12:00:00Z";

#[test]
fn daemon_status_not_running_human() {
    let cli = Cli::parse_from(["cfgd"]);
    let (printer, cap) = Printer::for_test_doc();
    cmd_daemon_status(&cli, &printer).unwrap();
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "daemon_status/not_running.txt");
}

#[test]
fn daemon_status_not_running_json() {
    let cli = Cli::parse_from(["cfgd"]);
    let (printer, cap) = Printer::for_test_doc();
    cmd_daemon_status(&cli, &printer).unwrap();
    drop(printer);
    let json = cap.json().expect("doc captured json");
    assert_eq!(json["running"], false);
    assert_eq!(json["pid"], 0);
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "daemon_status/not_running.json");
}

#[test]
fn daemon_status_running_human() {
    let (printer, cap) = Printer::for_test_doc();
    let status = sample_status_basic();
    printer.emit(build_daemon_status_doc(
        Some(&status),
        &declared_sources(),
        DAEMON_STATUS_NOW,
    ));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "daemon_status/running.txt");
}

#[test]
fn daemon_status_running_json() {
    let (printer, cap) = Printer::for_test_doc();
    let status = sample_status_basic();
    printer.emit(build_daemon_status_doc(
        Some(&status),
        &declared_sources(),
        DAEMON_STATUS_NOW,
    ));
    drop(printer);
    let json = cap.json().expect("doc captured json");
    assert_eq!(json["pid"], 4242);
    assert_eq!(json["uptimeSecs"], 3600);
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "daemon_status/running.json");
}

#[test]
fn daemon_status_running_no_timestamps_human() {
    let (printer, cap) = Printer::for_test_doc();
    let status = sample_status_no_timestamps();
    printer.emit(build_daemon_status_doc(
        Some(&status),
        &[],
        DAEMON_STATUS_NOW,
    ));
    drop(printer);
    cap.assert_human_snapshot_in(
        Path::new(SNAPSHOT_ROOT),
        "daemon_status/running_no_timestamps.txt",
    );
}

#[test]
fn daemon_status_running_with_update_human() {
    let (printer, cap) = Printer::for_test_doc();
    let status = sample_status_with_update();
    printer.emit(build_daemon_status_doc(
        Some(&status),
        &declared_sources(),
        DAEMON_STATUS_NOW,
    ));
    drop(printer);
    cap.assert_human_snapshot_in(
        Path::new(SNAPSHOT_ROOT),
        "daemon_status/running_with_update.txt",
    );
}

// --- cfgd daemon install ---------------------------------------------------

fn install_payload_linux() -> DaemonInstallOutput {
    DaemonInstallOutput {
        platform: "linux".to_string(),
        service: "cfgd.service".to_string(),
        path: "~/.config/systemd/user/cfgd.service".to_string(),
        started: false,
        windows_event_log: None,
    }
}

fn install_payload_macos() -> DaemonInstallOutput {
    DaemonInstallOutput {
        platform: "macos".to_string(),
        service: "com.cfgd.daemon".to_string(),
        path: "~/Library/LaunchAgents/com.cfgd.daemon.plist".to_string(),
        started: false,
        windows_event_log: None,
    }
}

fn install_payload_windows() -> DaemonInstallOutput {
    DaemonInstallOutput {
        platform: "windows".to_string(),
        service: "cfgd".to_string(),
        path: "%LOCALAPPDATA%\\cfgd\\daemon.log".to_string(),
        started: true,
        windows_event_log: Some(false),
    }
}

#[test]
fn daemon_install_installed_linux_human() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_daemon_install_doc(&install_payload_linux()));
    drop(printer);
    cap.assert_human_snapshot_in(
        Path::new(SNAPSHOT_ROOT),
        "daemon_install/installed_linux.txt",
    );
}

#[test]
fn daemon_install_installed_linux_json() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_daemon_install_doc(&install_payload_linux()));
    drop(printer);
    let json = cap.json().expect("doc captured json");
    assert_eq!(json["platform"], "linux");
    assert_eq!(json["service"], "cfgd.service");
    cap.assert_json_snapshot_in(
        Path::new(SNAPSHOT_ROOT),
        "daemon_install/installed_linux.json",
    );
}

#[test]
fn daemon_install_installed_macos_human() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_daemon_install_doc(&install_payload_macos()));
    drop(printer);
    cap.assert_human_snapshot_in(
        Path::new(SNAPSHOT_ROOT),
        "daemon_install/installed_macos.txt",
    );
}

#[test]
fn daemon_install_installed_macos_json() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_daemon_install_doc(&install_payload_macos()));
    drop(printer);
    let json = cap.json().expect("doc captured json");
    assert_eq!(json["platform"], "macos");
    assert_eq!(json["service"], "com.cfgd.daemon");
    cap.assert_json_snapshot_in(
        Path::new(SNAPSHOT_ROOT),
        "daemon_install/installed_macos.json",
    );
}

#[test]
fn daemon_install_installed_windows_human() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_daemon_install_doc(&install_payload_windows()));
    drop(printer);
    cap.assert_human_snapshot_in(
        Path::new(SNAPSHOT_ROOT),
        "daemon_install/installed_windows.txt",
    );
}

#[test]
fn daemon_install_installed_windows_json() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_daemon_install_doc(&install_payload_windows()));
    drop(printer);
    let json = cap.json().expect("doc captured json");
    assert_eq!(json["platform"], "windows");
    assert_eq!(json["service"], "cfgd");
    assert_eq!(json["windowsEventLog"], false);
    cap.assert_json_snapshot_in(
        Path::new(SNAPSHOT_ROOT),
        "daemon_install/installed_windows.json",
    );
}

#[test]
fn daemon_install_install_failed_human() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(cfgd_core::output::error_doc(
        "cfgd",
        "install_failed",
        "Failed to install daemon service: permission denied",
        serde_json::json!({ "platform": "linux", "service": "cfgd.service" }),
    ));
    drop(printer);
    cap.assert_human_snapshot_in(
        Path::new(SNAPSHOT_ROOT),
        "daemon_install/install_failed.txt",
    );
}

#[test]
fn daemon_install_install_failed_json() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(cfgd_core::output::error_doc(
        "cfgd",
        "install_failed",
        "Failed to install daemon service: permission denied",
        serde_json::json!({ "platform": "linux", "service": "cfgd.service" }),
    ));
    drop(printer);
    let json = cap.json().expect("doc captured json");
    assert_eq!(json["error"], "install_failed");
    assert_eq!(json["name"], "cfgd");
    assert_eq!(json["platform"], "linux");
    assert_eq!(json["service"], "cfgd.service");
    cap.assert_json_snapshot_in(
        Path::new(SNAPSHOT_ROOT),
        "daemon_install/install_failed.json",
    );
}

// --- cfgd daemon uninstall -------------------------------------------------

fn uninstall_payload(platform: &str, service: &str) -> DaemonUninstallOutput {
    DaemonUninstallOutput {
        platform: platform.to_string(),
        service: service.to_string(),
        removed: true,
    }
}

#[test]
fn daemon_uninstall_linux_human() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_daemon_uninstall_doc(
        &uninstall_payload("linux", "cfgd.service"),
        cfgd_core::Scope::User,
    ));
    drop(printer);
    cap.assert_human_snapshot_in(
        Path::new(SNAPSHOT_ROOT),
        "daemon_uninstall/uninstalled_linux.txt",
    );
}

#[test]
fn daemon_uninstall_linux_json() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_daemon_uninstall_doc(
        &uninstall_payload("linux", "cfgd.service"),
        cfgd_core::Scope::User,
    ));
    drop(printer);
    let json = cap.json().expect("doc captured json");
    assert_eq!(json["platform"], "linux");
    assert_eq!(json["removed"], true);
    cap.assert_json_snapshot_in(
        Path::new(SNAPSHOT_ROOT),
        "daemon_uninstall/uninstalled_linux.json",
    );
}

#[test]
fn daemon_uninstall_macos_human() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_daemon_uninstall_doc(
        &uninstall_payload("macos", "com.cfgd.daemon"),
        cfgd_core::Scope::User,
    ));
    drop(printer);
    cap.assert_human_snapshot_in(
        Path::new(SNAPSHOT_ROOT),
        "daemon_uninstall/uninstalled_macos.txt",
    );
}

#[test]
fn daemon_uninstall_macos_json() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_daemon_uninstall_doc(
        &uninstall_payload("macos", "com.cfgd.daemon"),
        cfgd_core::Scope::User,
    ));
    drop(printer);
    let json = cap.json().expect("doc captured json");
    assert_eq!(json["platform"], "macos");
    assert_eq!(json["service"], "com.cfgd.daemon");
    cap.assert_json_snapshot_in(
        Path::new(SNAPSHOT_ROOT),
        "daemon_uninstall/uninstalled_macos.json",
    );
}

#[test]
fn daemon_uninstall_windows_human() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_daemon_uninstall_doc(
        &uninstall_payload("windows", "cfgd"),
        cfgd_core::Scope::User,
    ));
    drop(printer);
    cap.assert_human_snapshot_in(
        Path::new(SNAPSHOT_ROOT),
        "daemon_uninstall/uninstalled_windows.txt",
    );
}

#[test]
fn daemon_uninstall_windows_json() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_daemon_uninstall_doc(
        &uninstall_payload("windows", "cfgd"),
        cfgd_core::Scope::User,
    ));
    drop(printer);
    let json = cap.json().expect("doc captured json");
    assert_eq!(json["platform"], "windows");
    assert_eq!(json["service"], "cfgd");
    cap.assert_json_snapshot_in(
        Path::new(SNAPSHOT_ROOT),
        "daemon_uninstall/uninstalled_windows.json",
    );
}

#[test]
fn daemon_uninstall_uninstall_failed_human() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(cfgd_core::output::error_doc(
        "cfgd",
        "uninstall_failed",
        "Failed to uninstall daemon service: unit file not found",
        serde_json::json!({ "platform": "linux", "service": "cfgd.service" }),
    ));
    drop(printer);
    cap.assert_human_snapshot_in(
        Path::new(SNAPSHOT_ROOT),
        "daemon_uninstall/uninstall_failed.txt",
    );
}

#[test]
fn daemon_uninstall_uninstall_failed_json() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(cfgd_core::output::error_doc(
        "cfgd",
        "uninstall_failed",
        "Failed to uninstall daemon service: unit file not found",
        serde_json::json!({ "platform": "linux", "service": "cfgd.service" }),
    ));
    drop(printer);
    let json = cap.json().expect("doc captured json");
    assert_eq!(json["error"], "uninstall_failed");
    assert_eq!(json["platform"], "linux");
    cap.assert_json_snapshot_in(
        Path::new(SNAPSHOT_ROOT),
        "daemon_uninstall/uninstall_failed.json",
    );
}
