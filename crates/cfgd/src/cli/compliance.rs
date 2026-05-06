use super::*;

/// Collect a compliance snapshot, hash it, and store in the state store.
/// Shared setup used by both `cmd_compliance_snapshot` and `cmd_compliance_export`.
pub(super) fn collect_and_store_compliance_snapshot(
    cli: &Cli,
    printer: &Printer,
) -> anyhow::Result<(CfgdConfig, cfgd_core::compliance::ComplianceSnapshot)> {
    let (cfg, mut resolved) = load_config_and_profile(cli, printer)?;
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

/// Build a snapshot and print summary table; optionally store in state.
pub(super) fn cmd_compliance_snapshot(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    let (_cfg, snapshot) = collect_and_store_compliance_snapshot(cli, printer)?;

    if printer.is_structured() {
        printer.write_structured(&ComplianceSnapshotOutput {
            snapshot: snapshot.clone(),
        });
    } else {
        print_compliance_summary(&snapshot, printer);
    }

    Ok(())
}

/// Export snapshot to configured export path (or stdout if -o json).
pub(super) fn cmd_compliance_export(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    let (cfg, snapshot) = collect_and_store_compliance_snapshot(cli, printer)?;

    if printer.is_structured() {
        printer.write_structured(&ComplianceSnapshotOutput {
            snapshot: snapshot.clone(),
        });
        return Ok(());
    }

    let export = cfg
        .spec
        .compliance
        .as_ref()
        .map(|c| c.export.clone())
        .unwrap_or_default();

    let export_path = cfgd_core::compliance::export_snapshot_to_file(&snapshot, &export)?;
    printer.success(&format!(
        "Compliance snapshot written to {}",
        export_path.display()
    ));
    print_compliance_summary(&snapshot, printer);

    Ok(())
}

/// Show compliance snapshot history.
pub(super) fn cmd_compliance_history(
    cli: &Cli,
    printer: &Printer,
    since: Option<&str>,
) -> anyhow::Result<()> {
    let state = open_state_store(cli.state_dir.as_deref())?;

    // Convert --since duration to an ISO 8601 timestamp cutoff.
    let since_ts: Option<String> = since
        .map(|s| {
            let dur = cfgd_core::parse_duration_str(s)
                .map_err(|e| anyhow::anyhow!("invalid --since value '{}': {}", s, e))?;
            let cutoff_secs = cfgd_core::unix_secs_now().saturating_sub(dur.as_secs());
            Ok::<String, anyhow::Error>(cfgd_core::unix_secs_to_iso8601(cutoff_secs))
        })
        .transpose()?;

    let entries = state.compliance_history(since_ts.as_deref(), 100)?;

    if printer.is_structured() {
        printer.write_structured(&ComplianceHistoryOutput {
            entries: entries.clone(),
        });
        return Ok(());
    }

    printer.header("Compliance History");

    if entries.is_empty() {
        printer.newline();
        printer.info("No compliance snapshots recorded yet");
        return Ok(());
    }

    printer.newline();
    printer.table(
        &["ID", "Timestamp", "Compliant", "Warning", "Violation"],
        &entries
            .iter()
            .map(|row| {
                vec![
                    row.id.to_string(),
                    row.timestamp.clone(),
                    row.compliant.to_string(),
                    row.warning.to_string(),
                    row.violation.to_string(),
                ]
            })
            .collect::<Vec<_>>(),
    );

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

/// Print a human-readable compliance summary table.
pub(super) fn print_compliance_summary(
    snapshot: &cfgd_core::compliance::ComplianceSnapshot,
    printer: &Printer,
) {
    printer.header("Compliance Snapshot");
    printer.newline();
    printer.key_value("Timestamp", &snapshot.timestamp);
    printer.key_value("Machine", &snapshot.machine.hostname);
    printer.key_value("Profile", &snapshot.profile);
    printer.newline();

    if snapshot.checks.is_empty() {
        printer.info("No checks performed");
        return;
    }

    // Group by category for the summary table
    use std::collections::BTreeMap;
    let mut by_category: BTreeMap<&str, (usize, usize, usize)> = BTreeMap::new();
    for check in &snapshot.checks {
        let entry = by_category.entry(check.category.as_str()).or_default();
        match check.status {
            cfgd_core::compliance::ComplianceStatus::Compliant => entry.0 += 1,
            cfgd_core::compliance::ComplianceStatus::Warning => entry.1 += 1,
            cfgd_core::compliance::ComplianceStatus::Violation => entry.2 += 1,
        }
    }

    printer.table(
        &["Category", "Compliant", "Warning", "Violation"],
        &by_category
            .iter()
            .map(|(cat, (c, w, v))| {
                vec![cat.to_string(), c.to_string(), w.to_string(), v.to_string()]
            })
            .collect::<Vec<_>>(),
    );

    printer.newline();
    let s = &snapshot.summary;
    if s.violation > 0 {
        printer.error(&format!(
            "Summary: {} compliant, {} warning, {} violation",
            s.compliant, s.warning, s.violation
        ));
    } else if s.warning > 0 {
        printer.warning(&format!(
            "Summary: {} compliant, {} warning, {} violation",
            s.compliant, s.warning, s.violation
        ));
    } else {
        printer.success(&format!("All {} check(s) compliant", s.compliant));
    }
}
