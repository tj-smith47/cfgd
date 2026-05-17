use super::*;
use cfgd_core::compliance::{ComplianceCheck, ComplianceSnapshot, ComplianceStatus};
use cfgd_core::output_v2::{Doc, Printer as PrinterV2, Role, renderer::Table as TableV2};
use cfgd_core::state::ComplianceHistoryRow;

/// Collect a compliance snapshot, hash it, and store in the state store.
/// Shared setup used by both `cmd_compliance_snapshot` and `cmd_compliance_export`.
pub(super) fn collect_and_store_compliance_snapshot(
    cli: &Cli,
) -> anyhow::Result<(CfgdConfig, ComplianceSnapshot)> {
    let (cfg, _profile_name, mut resolved) = helpers::load_config_and_profile_v2(cli)?;
    let config_dir = config_dir(cli);
    packages::resolve_manifest_packages(&mut resolved.merged.packages, &config_dir)?;
    let registry = build_registry_with_profile(&resolved.merged.packages);

    let profile_name = cli
        .profile
        .as_deref()
        .unwrap_or_else(|| cfg.active_profile().unwrap_or("default"));

    let scope = cfg
        .spec
        .compliance
        .as_ref()
        .map(|c| c.scope.clone())
        .unwrap_or_default();

    let sources: Vec<String> = cfg.spec.sources.iter().map(|s| s.name.clone()).collect();

    let snapshot = cfgd_core::compliance::collect_snapshot(
        profile_name,
        &resolved.merged,
        &registry,
        &scope,
        &sources,
    )?;

    let state = open_state_store(cli.state_dir.as_deref())?;
    let json = serde_json::to_string(&snapshot).map_err(|e| anyhow::anyhow!("serialize: {}", e))?;
    let hash = cfgd_core::sha256_hex(json.as_bytes());
    state.store_compliance_snapshot(&snapshot, &hash)?;

    Ok((cfg, snapshot))
}

/// Build a snapshot and emit a v2 summary Doc.
pub(super) fn cmd_compliance_snapshot(cli: &Cli, v2_printer: &PrinterV2) -> anyhow::Result<()> {
    let (_cfg, snapshot) = collect_and_store_compliance_snapshot(cli)?;
    v2_printer.emit(build_compliance_summary_doc(&snapshot));
    Ok(())
}

/// Export snapshot to configured export path and emit a v2 summary Doc.
pub(super) fn cmd_compliance_export(cli: &Cli, v2_printer: &PrinterV2) -> anyhow::Result<()> {
    let (cfg, snapshot) = collect_and_store_compliance_snapshot(cli)?;

    let export = cfg
        .spec
        .compliance
        .as_ref()
        .map(|c| c.export.clone())
        .unwrap_or_default();

    let export_path = cfgd_core::compliance::export_snapshot_to_file(&snapshot, &export)?;
    v2_printer.emit(build_compliance_export_doc(&snapshot, &export_path));
    Ok(())
}

/// Show compliance snapshot history.
pub(super) fn cmd_compliance_history(
    cli: &Cli,
    v2_printer: &PrinterV2,
    since: Option<&str>,
) -> anyhow::Result<()> {
    let state = open_state_store(cli.state_dir.as_deref())?;

    let since_ts: Option<String> = since
        .map(|s| {
            let dur = cfgd_core::parse_duration_str(s)
                .map_err(|e| anyhow::anyhow!("invalid --since value '{}': {}", s, e))?;
            let cutoff_secs = cfgd_core::unix_secs_now().saturating_sub(dur.as_secs());
            Ok::<String, anyhow::Error>(cfgd_core::unix_secs_to_iso8601(cutoff_secs))
        })
        .transpose()?;

    let entries = state.compliance_history(since_ts.as_deref(), 100)?;
    v2_printer.emit(build_compliance_history_doc(&entries));
    Ok(())
}

/// Show diff between two snapshots by ID.
pub(super) fn cmd_compliance_diff(
    cli: &Cli,
    printer: &Printer,
    id1: i64,
    id2: i64,
) -> anyhow::Result<()> {
    let state = open_state_store(cli.state_dir.as_deref())?;

    let snap1 = state
        .get_compliance_snapshot(id1)?
        .ok_or_else(|| anyhow::anyhow!("snapshot #{} not found", id1))?;
    let snap2 = state
        .get_compliance_snapshot(id2)?
        .ok_or_else(|| anyhow::anyhow!("snapshot #{} not found", id2))?;

    // Build a key for each check to match them between snapshots.
    fn check_key(c: &cfgd_core::compliance::ComplianceCheck) -> String {
        let id = c
            .target
            .as_deref()
            .or(c.name.as_deref())
            .or(c.key.as_deref())
            .or(c.path.as_deref())
            .unwrap_or("(unknown)");
        format!("{}:{}", c.category, id)
    }

    use std::collections::HashMap;
    let map1: HashMap<String, &cfgd_core::compliance::ComplianceCheck> =
        snap1.checks.iter().map(|c| (check_key(c), c)).collect();
    let map2: HashMap<String, &cfgd_core::compliance::ComplianceCheck> =
        snap2.checks.iter().map(|c| (check_key(c), c)).collect();

    let mut added: Vec<cfgd_core::compliance::ComplianceCheck> = Vec::new();
    let mut removed: Vec<cfgd_core::compliance::ComplianceCheck> = Vec::new();
    let mut changed: Vec<ComplianceCheckChange> = Vec::new();

    for (key, check2) in &map2 {
        if let Some(check1) = map1.get(key) {
            if check1.status != check2.status {
                changed.push(ComplianceCheckChange {
                    key: key.clone(),
                    old_status: format!("{:?}", check1.status),
                    new_status: format!("{:?}", check2.status),
                    detail: check2.detail.clone(),
                });
            }
        } else {
            added.push((*check2).clone());
        }
    }
    for (key, check1) in &map1 {
        if !map2.contains_key(key) {
            removed.push((*check1).clone());
        }
    }

    // Sort for deterministic output
    added.sort_by_key(check_key);
    removed.sort_by_key(check_key);
    changed.sort_by(|a, b| a.key.cmp(&b.key));

    if printer.is_structured() {
        printer.write_structured(&ComplianceDiffOutput {
            id1,
            id2,
            added: added.clone(),
            removed: removed.clone(),
            changed: changed.clone(),
        });
        return Ok(());
    }

    printer.header(&format!("Compliance Diff #{} → #{}", id1, id2));
    printer.newline();
    printer.key_value("Snapshot 1", &snap1.timestamp);
    printer.key_value("Snapshot 2", &snap2.timestamp);
    printer.newline();

    if added.is_empty() && removed.is_empty() && changed.is_empty() {
        printer.success("No differences between snapshots");
        return Ok(());
    }

    if !added.is_empty() {
        printer.subheader(&format!("Added ({} check(s))", added.len()));
        for check in &added {
            printer.success(&format!("  + {}", check_key(check)));
        }
        printer.newline();
    }

    if !removed.is_empty() {
        printer.subheader(&format!("Removed ({} check(s))", removed.len()));
        for check in &removed {
            printer.warning(&format!("  - {}", check_key(check)));
        }
        printer.newline();
    }

    if !changed.is_empty() {
        printer.subheader(&format!("Changed ({} check(s))", changed.len()));
        for change in &changed {
            let msg = format!(
                "  ~ {} ({} → {})",
                change.key, change.old_status, change.new_status
            );
            if change.new_status == "Violation" {
                printer.error(&msg);
            } else if change.new_status == "Warning" {
                printer.warning(&msg);
            } else {
                printer.success(&msg);
            }
            if let Some(ref detail) = change.detail {
                printer.info(&format!("    {}", detail));
            }
        }
    }

    Ok(())
}

/// Pure builder: compliance snapshot summary Doc.
pub fn build_compliance_summary_doc(snapshot: &ComplianceSnapshot) -> Doc {
    let overall = overall_status(&snapshot.summary);

    let mut doc = Doc::new().heading("Compliance Summary").kv_block([
        ("Timestamp", snapshot.timestamp.clone()),
        ("Machine", snapshot.machine.hostname.clone()),
        ("Profile", snapshot.profile.clone()),
        ("Status", overall.to_string()),
    ]);

    doc = doc.kv_block([
        ("Compliant", snapshot.summary.compliant.to_string()),
        ("Warning", snapshot.summary.warning.to_string()),
        ("Violation", snapshot.summary.violation.to_string()),
    ]);

    if snapshot.checks.is_empty() {
        doc = doc.status(Role::Info, "No checks performed");
        return doc.with_data(ComplianceSnapshotOutput {
            snapshot: snapshot.clone(),
        });
    }

    let violations: Vec<&ComplianceCheck> = snapshot
        .checks
        .iter()
        .filter(|c| c.status == ComplianceStatus::Violation)
        .collect();
    doc = doc.section_if_nonempty("Violations", &violations, |s, items| {
        items.iter().fold(s, |s, c| {
            s.status_with(Role::Fail, check_subject(c), |sf| {
                sf.detail_opt(c.detail.as_deref())
            })
        })
    });

    let warnings: Vec<&ComplianceCheck> = snapshot
        .checks
        .iter()
        .filter(|c| c.status == ComplianceStatus::Warning)
        .collect();
    doc = doc.section_if_nonempty("Warnings", &warnings, |s, items| {
        items.iter().fold(s, |s, c| {
            s.status_with(Role::Warn, check_subject(c), |sf| {
                sf.detail_opt(c.detail.as_deref())
            })
        })
    });

    let role = if snapshot.summary.violation > 0 {
        Role::Fail
    } else if snapshot.summary.warning > 0 {
        Role::Warn
    } else {
        Role::Ok
    };
    let summary_line = if snapshot.summary.violation > 0 || snapshot.summary.warning > 0 {
        format!(
            "Summary: {} compliant, {} warning, {} violation",
            snapshot.summary.compliant, snapshot.summary.warning, snapshot.summary.violation
        )
    } else {
        format!("All {} check(s) compliant", snapshot.summary.compliant)
    };
    doc = doc.status(role, summary_line);

    doc.with_data(ComplianceSnapshotOutput {
        snapshot: snapshot.clone(),
    })
}

/// Pure builder: compliance export Doc (success status + summary).
pub fn build_compliance_export_doc(
    snapshot: &ComplianceSnapshot,
    export_path: &std::path::Path,
) -> Doc {
    Doc::new()
        .heading("Compliance Export")
        .status(
            Role::Ok,
            format!("Compliance snapshot written to {}", export_path.display()),
        )
        .kv_block([
            ("Compliant", snapshot.summary.compliant.to_string()),
            ("Warning", snapshot.summary.warning.to_string()),
            ("Violation", snapshot.summary.violation.to_string()),
        ])
        .with_data(ComplianceSnapshotOutput {
            snapshot: snapshot.clone(),
        })
}

/// Pure builder: compliance history Doc (table or empty-state).
pub fn build_compliance_history_doc(entries: &[ComplianceHistoryRow]) -> Doc {
    let mut doc = Doc::new().heading("Compliance History");
    if entries.is_empty() {
        doc = doc.status(Role::Info, "No compliance snapshots recorded yet");
    } else {
        let mut table = TableV2::new(["ID", "Timestamp", "Compliant", "Warning", "Violation"]);
        for row in entries {
            table = table.row([
                row.id.to_string(),
                row.timestamp.clone(),
                row.compliant.to_string(),
                row.warning.to_string(),
                row.violation.to_string(),
            ]);
        }
        doc = doc.table(table);
    }
    doc.with_data(ComplianceHistoryOutput {
        entries: entries.to_vec(),
    })
}

/// Derive an overall-status label from a `ComplianceSummary`.
fn overall_status(summary: &cfgd_core::compliance::ComplianceSummary) -> &'static str {
    if summary.violation > 0 {
        "Violation"
    } else if summary.warning > 0 {
        "Warning"
    } else {
        "Compliant"
    }
}

/// Subject line for a check — picks the first available identifier and
/// prefixes the category, matching the diff/key convention.
fn check_subject(c: &ComplianceCheck) -> String {
    let id = c
        .target
        .as_deref()
        .or(c.name.as_deref())
        .or(c.key.as_deref())
        .or(c.path.as_deref())
        .unwrap_or("(unknown)");
    format!("{}:{}", c.category, id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cfgd_core::compliance::{
        ComplianceCheck, ComplianceSnapshot, ComplianceStatus, ComplianceSummary, MachineInfo,
    };
    use cfgd_core::output::OutputFormat;

    fn sample_snapshot(checks: Vec<ComplianceCheck>) -> ComplianceSnapshot {
        let summary = cfgd_core::compliance::compute_summary(&checks);
        ComplianceSnapshot {
            timestamp: "2026-05-12T00:00:00Z".into(),
            machine: MachineInfo {
                hostname: "test-host".into(),
                os: "linux".into(),
                arch: "x86_64".into(),
            },
            profile: "default".into(),
            sources: vec!["local".into()],
            checks,
            summary,
        }
    }

    fn check(category: &str, target: &str, status: ComplianceStatus) -> ComplianceCheck {
        ComplianceCheck {
            category: category.into(),
            target: Some(target.into()),
            status,
            ..Default::default()
        }
    }

    fn test_cli_for(state_dir: &std::path::Path) -> Cli {
        Cli {
            config: state_dir.join("cfgd.yaml"),
            profile: None,
            verbose: 0,
            quiet: true,
            no_color: true,
            output: OutputFormatArg(OutputFormat::Table),
            jsonpath: None,
            state_dir: Some(state_dir.to_path_buf()),
            command: None,
        }
    }

    fn store_snapshot(state_dir: &std::path::Path, snapshot: &ComplianceSnapshot) {
        let state = open_state_store(Some(state_dir)).unwrap();
        let json = serde_json::to_string(snapshot).unwrap();
        let hash = cfgd_core::sha256_hex(json.as_bytes());
        state.store_compliance_snapshot(snapshot, &hash).unwrap();
    }

    // --- build_compliance_summary_doc ---

    #[test]
    fn build_compliance_summary_doc_all_compliant() {
        let snapshot = sample_snapshot(vec![
            check("file", "/etc/hosts", ComplianceStatus::Compliant),
            check("package", "ripgrep", ComplianceStatus::Compliant),
        ]);
        let (printer, cap) = PrinterV2::for_test_doc();
        printer.emit(build_compliance_summary_doc(&snapshot));
        drop(printer);

        let output = cap.human();
        assert!(
            output.contains("Compliance Summary"),
            "should print heading, got: {output}"
        );
        assert!(
            output.contains("test-host"),
            "should print hostname, got: {output}"
        );
        assert!(
            output.contains("All 2 check(s) compliant"),
            "should print all-compliant summary, got: {output}"
        );
    }

    #[test]
    fn build_compliance_summary_doc_warning_route() {
        let snapshot = sample_snapshot(vec![
            check("file", "/etc/a", ComplianceStatus::Compliant),
            check("system", "sysctl.x", ComplianceStatus::Warning),
        ]);
        let (printer, cap) = PrinterV2::for_test_doc();
        printer.emit(build_compliance_summary_doc(&snapshot));
        drop(printer);

        let output = cap.human();
        assert!(
            output.contains("Summary: 1 compliant, 1 warning, 0 violation"),
            "should take warning summary route, got: {output}"
        );
        assert!(
            output.contains("Warnings"),
            "should render Warnings section, got: {output}"
        );
    }

    #[test]
    fn build_compliance_summary_doc_violation_route() {
        let snapshot = sample_snapshot(vec![
            check("file", "/etc/a", ComplianceStatus::Compliant),
            check("file", "/etc/b", ComplianceStatus::Warning),
            check("package", "ripgrep", ComplianceStatus::Violation),
        ]);
        let (printer, cap) = PrinterV2::for_test_doc();
        printer.emit(build_compliance_summary_doc(&snapshot));
        drop(printer);

        let output = cap.human();
        assert!(
            output.contains("Summary: 1 compliant, 1 warning, 1 violation"),
            "should take violation summary route, got: {output}"
        );
        assert!(
            output.contains("Violations"),
            "should render Violations section, got: {output}"
        );
    }

    #[test]
    fn build_compliance_summary_doc_empty_checks() {
        let snapshot = sample_snapshot(vec![]);
        let (printer, cap) = PrinterV2::for_test_doc();
        printer.emit(build_compliance_summary_doc(&snapshot));
        drop(printer);

        let output = cap.human();
        assert!(
            output.contains("No checks performed"),
            "empty checks should print empty-state, got: {output}"
        );
        assert!(
            !output.contains("Summary:"),
            "empty checks should not print summary line, got: {output}"
        );
    }

    // --- cmd_compliance_diff (unchanged — T3 owns) ---

    #[test]
    fn cmd_compliance_diff_missing_snapshots_returns_err() {
        let state_dir = tempfile::tempdir().unwrap();
        let cli = test_cli_for(state_dir.path());
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);

        let err = cmd_compliance_diff(&cli, &printer, 1, 2).unwrap_err();
        assert!(
            err.to_string().contains("not found"),
            "expected 'not found' error, got: {}",
            err
        );
    }

    #[test]
    fn cmd_compliance_diff_no_differences_when_snapshots_equal() {
        let state_dir = tempfile::tempdir().unwrap();
        let snapshot = sample_snapshot(vec![check(
            "file",
            "/etc/hosts",
            ComplianceStatus::Compliant,
        )]);
        store_snapshot(state_dir.path(), &snapshot);
        store_snapshot(state_dir.path(), &snapshot);

        let cli = test_cli_for(state_dir.path());
        let (printer, buf) = Printer::for_test();

        cmd_compliance_diff(&cli, &printer, 1, 2).unwrap();

        let output = buf.lock().unwrap();
        assert!(
            output.contains("No differences between snapshots"),
            "identical snapshots should print no-diff message, got: {output}"
        );
    }

    #[test]
    fn cmd_compliance_diff_added_removed_changed_branches() {
        let state_dir = tempfile::tempdir().unwrap();

        let snap1 = sample_snapshot(vec![
            check("file", "/a", ComplianceStatus::Compliant),
            check("file", "/b", ComplianceStatus::Compliant),
        ]);
        let snap2 = sample_snapshot(vec![
            check("file", "/a", ComplianceStatus::Violation),
            check("file", "/c", ComplianceStatus::Warning),
        ]);
        store_snapshot(state_dir.path(), &snap1);
        store_snapshot(state_dir.path(), &snap2);

        let cli = test_cli_for(state_dir.path());
        let (printer, buf) = Printer::for_test();

        cmd_compliance_diff(&cli, &printer, 1, 2).unwrap();

        let output = buf.lock().unwrap();
        assert!(
            output.contains("Added (1 check(s))") && output.contains("file:/c"),
            "should report added check file:/c, got: {output}"
        );
        assert!(
            output.contains("Removed (1 check(s))") && output.contains("file:/b"),
            "should report removed check file:/b, got: {output}"
        );
        assert!(
            output.contains("Changed (1 check(s))") && output.contains("file:/a"),
            "should report changed check file:/a, got: {output}"
        );
        assert!(
            output.contains("Compliant") && output.contains("Violation"),
            "changed line should include old + new status, got: {output}"
        );
    }

    #[test]
    fn cmd_compliance_diff_structured_json_output() {
        let state_dir = tempfile::tempdir().unwrap();
        let snap1 = sample_snapshot(vec![check("file", "/a", ComplianceStatus::Compliant)]);
        let snap2 = sample_snapshot(vec![
            check("file", "/a", ComplianceStatus::Violation),
            check("file", "/b", ComplianceStatus::Compliant),
        ]);
        store_snapshot(state_dir.path(), &snap1);
        store_snapshot(state_dir.path(), &snap2);

        let mut cli = test_cli_for(state_dir.path());
        cli.output = OutputFormatArg(OutputFormat::Json);
        let (printer, buf) = Printer::for_test_with_format(OutputFormat::Json);

        cmd_compliance_diff(&cli, &printer, 1, 2).unwrap();

        let output = buf.lock().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(output.trim())
            .unwrap_or_else(|e| panic!("invalid JSON: {e}, got: {output}"));
        assert_eq!(parsed["id1"], 1);
        assert_eq!(parsed["id2"], 2);
        assert!(
            parsed["added"].is_array() && parsed["added"].as_array().unwrap().len() == 1,
            "expected exactly 1 added entry, got: {parsed}"
        );
        assert!(
            parsed["removed"].is_array() && parsed["removed"].as_array().unwrap().is_empty(),
            "expected no removed entries, got: {parsed}"
        );
        let changed = parsed["changed"].as_array().expect("changed array");
        assert_eq!(changed.len(), 1, "expected 1 changed entry, got: {parsed}");
        assert_eq!(changed[0]["key"], "file:/a");
        assert_eq!(changed[0]["newStatus"], "Violation");
    }

    // --- cmd_compliance_history ---

    #[test]
    fn cmd_compliance_history_empty_state_prints_no_snapshots() {
        let state_dir = tempfile::tempdir().unwrap();
        let cli = test_cli_for(state_dir.path());
        let (printer, cap) = PrinterV2::for_test_doc();

        cmd_compliance_history(&cli, &printer, None).unwrap();
        drop(printer);

        let output = cap.human();
        assert!(
            output.contains("No compliance snapshots recorded yet"),
            "should print empty-state message, got: {output}"
        );
    }

    #[test]
    fn cmd_compliance_history_invalid_since_returns_err() {
        let state_dir = tempfile::tempdir().unwrap();
        let cli = test_cli_for(state_dir.path());
        let (printer, _cap) = PrinterV2::for_test_doc();

        let err = cmd_compliance_history(&cli, &printer, Some("not-a-duration")).unwrap_err();
        assert!(
            err.to_string().contains("invalid --since value"),
            "expected 'invalid --since value', got: {}",
            err
        );
    }

    #[test]
    fn cmd_compliance_history_after_seed_renders_table() {
        let state_dir = tempfile::tempdir().unwrap();
        let snapshot = sample_snapshot(vec![
            check("file", "/etc/hosts", ComplianceStatus::Compliant),
            check("package", "ripgrep", ComplianceStatus::Violation),
        ]);
        store_snapshot(state_dir.path(), &snapshot);

        let cli = test_cli_for(state_dir.path());
        let (printer, cap) = PrinterV2::for_test_doc();

        cmd_compliance_history(&cli, &printer, None).unwrap();
        drop(printer);

        let output = cap.human();
        assert!(
            output.contains("Compliance History"),
            "should print history heading, got: {output}"
        );
        assert!(
            output.contains("2026-05-12T00:00:00Z"),
            "should include seeded timestamp, got: {output}"
        );
    }

    #[test]
    fn cmd_compliance_history_structured_json_with_entry() {
        let state_dir = tempfile::tempdir().unwrap();
        let snapshot = sample_snapshot(vec![check(
            "file",
            "/etc/hosts",
            ComplianceStatus::Compliant,
        )]);
        store_snapshot(state_dir.path(), &snapshot);

        let cli = test_cli_for(state_dir.path());
        let (printer, cap) = PrinterV2::for_test_doc();

        cmd_compliance_history(&cli, &printer, None).unwrap();
        drop(printer);

        let parsed = cap.json().expect("history Doc carries with_data payload");
        let entries = parsed["entries"].as_array().expect("entries array");
        assert_eq!(entries.len(), 1, "expected 1 entry, got: {parsed}");
        assert_eq!(entries[0]["compliant"], 1);
        assert_eq!(entries[0]["violation"], 0);
    }

    // --- ComplianceSummary smoke: confirm sample_snapshot helper ---

    #[test]
    fn sample_snapshot_summary_matches_checks() {
        let snapshot = sample_snapshot(vec![
            check("file", "/a", ComplianceStatus::Compliant),
            check("file", "/b", ComplianceStatus::Warning),
            check("file", "/c", ComplianceStatus::Violation),
        ]);
        assert_eq!(
            (
                snapshot.summary.compliant,
                snapshot.summary.warning,
                snapshot.summary.violation
            ),
            (1, 1, 1)
        );
        let recomputed = ComplianceSummary {
            compliant: 1,
            warning: 1,
            violation: 1,
        };
        assert_eq!(snapshot.summary.compliant, recomputed.compliant);
        assert_eq!(snapshot.summary.warning, recomputed.warning);
        assert_eq!(snapshot.summary.violation, recomputed.violation);
    }
}
