use super::*;
use cfgd_core::PathDisplayExt;
use cfgd_core::compliance::{ComplianceCheck, ComplianceSnapshot, ComplianceStatus};
use cfgd_core::output::{Doc, Printer, Role, renderer::Table};
use cfgd_core::state::ComplianceHistoryRow;

/// Collect a compliance snapshot, hash it, and store in the state store.
/// Shared setup used by both `cmd_compliance_snapshot` and `cmd_compliance_export`.
pub(super) fn collect_and_store_compliance_snapshot<'a>(
    ctx: &'a RunContext<'_>,
) -> anyhow::Result<(&'a CfgdConfig, ComplianceSnapshot)> {
    let cli = ctx.cli();
    let printer = ctx.printer();
    let (cfg, _profile_name, local_resolved) = ctx.config_and_profile()?;
    let config_dir = ctx.config_dir();

    // Compose with sources (cache-only — read paths stay offline) and resolve the
    // effective module set through the one shared resolver, so the compliance
    // snapshot reflects the same source-composed desired state that `apply` writes.
    let quiet_printer = printer.at_verbosity(cfgd_core::output::Verbosity::Quiet);
    // Report mode: a source security-constraint violation surfaces as a compliance
    // check rather than aborting (exit 4). `compliance` reports state; it does not
    // gate on it — unlike apply/plan/daemon which compose in Enforce mode.
    let mut desired = resolve_desired_state(
        ctx,
        cfg,
        local_resolved,
        &[],
        false,
        &quiet_printer,
        false,
        composition::ConstraintMode::Report,
    )?;
    // Taken before the other fields, because a partial move out of `desired`
    // would block the `&mut self` this accessor needs.
    let mut registry = desired.take_registry(cfg);
    let constraint_violations = desired.constraint_violations;
    let mut resolved = desired.resolved;
    let resolved_modules = desired.modules;

    ctx.resolve_manifest_packages(&mut resolved.merged.packages)?;
    registry.file_manager = Some(Box::new(build_compliance_file_manager(
        config_dir,
        &resolved,
        Some(ctx),
    )?));

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

    let state = ctx.state()?;
    let mut snapshot = cfgd_core::compliance::collect_snapshot(
        profile_name,
        &resolved.merged,
        &resolved_modules,
        config_dir,
        &registry,
        &scope,
        &sources,
        &quiet_printer,
        state,
        None,
    )?;

    // Fold the Report-mode source-constraint violations into the snapshot as
    // Violation checks so they appear in the `checks` array and bump
    // `summary.violation`, then recompute the summary over the combined set.
    append_constraint_violation_checks(&mut snapshot, &constraint_violations);

    state.store_compliance_snapshot(&snapshot)?;

    Ok((cfg, snapshot))
}

/// Map a Report-mode source-constraint violation `kind` to a compliance check
/// category. Encryption constraints land in `file-encryption` (the category the
/// file-encryption compliance checks already use); other source constraints
/// share the `source-constraint` category.
fn constraint_violation_category(kind: &str) -> &'static str {
    match kind {
        "encryption-required" | "encryption-backend-mismatch" | "encryption-mode-mismatch" => {
            "file-encryption"
        }
        _ => "source-constraint",
    }
}

/// Append each Report-mode source-constraint violation to the snapshot as a
/// `Violation` check, then recompute the summary over the combined set. The
/// appended checks are sorted (category, then target/detail) for deterministic
/// output regardless of source-visit order.
fn append_constraint_violation_checks(
    snapshot: &mut ComplianceSnapshot,
    violations: &[cfgd_core::composition::ConstraintViolation],
) {
    if violations.is_empty() {
        return;
    }
    let mut extra: Vec<ComplianceCheck> = violations
        .iter()
        .map(|v| ComplianceCheck {
            category: constraint_violation_category(&v.kind).to_string(),
            target: v.path.clone(),
            status: ComplianceStatus::Violation,
            detail: Some(v.detail.clone()),
            ..Default::default()
        })
        .collect();
    extra.sort_by(|a, b| {
        a.category
            .cmp(&b.category)
            .then(a.target.cmp(&b.target))
            .then(a.detail.cmp(&b.detail))
    });
    snapshot.checks.extend(extra);
    snapshot.summary = cfgd_core::compliance::compute_summary(&snapshot.checks);
}

/// Build a snapshot and emit a compliance summary Doc.
pub(super) fn cmd_compliance_snapshot(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    let ctx = RunContext::new(cli, printer);
    let (_cfg, snapshot) = collect_and_store_compliance_snapshot(&ctx)?;
    printer.emit(build_compliance_summary_doc(&snapshot));
    Ok(())
}

/// Export snapshot to the configured export path and emit a compliance summary Doc.
pub(super) fn cmd_compliance_export(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    let ctx = RunContext::new(cli, printer);
    let (cfg, snapshot) = collect_and_store_compliance_snapshot(&ctx)?;

    let export = cfg
        .spec
        .compliance
        .as_ref()
        .map(|c| c.export.clone())
        .unwrap_or_default();

    let export_path = cfgd_core::compliance::export_snapshot_to_file(&snapshot, &export)?;
    printer.emit(build_compliance_export_doc(&snapshot, &export_path));
    Ok(())
}

/// Show compliance snapshot history.
pub(super) fn cmd_compliance_history(
    cli: &Cli,
    printer: &Printer,
    since: Option<&str>,
) -> anyhow::Result<()> {
    let state = open_state_store(cli.state_dir.as_deref(), cli.scope())?;

    let since_ts: Option<String> = since
        .map(|s| {
            let dur = cfgd_core::parse_duration_str(s)
                .map_err(|e| anyhow::anyhow!("invalid --since value '{}': {}", s, e))?;
            let cutoff_secs = cfgd_core::unix_secs_now().saturating_sub(dur.as_secs());
            Ok::<String, anyhow::Error>(cfgd_core::unix_secs_to_iso8601(cutoff_secs))
        })
        .transpose()?;

    let entries = state.compliance_history(since_ts.as_deref(), 100)?;
    printer.emit(build_compliance_history_doc(&entries));
    Ok(())
}

/// Show diff between two snapshots by ID.
pub(super) fn cmd_compliance_diff(
    cli: &Cli,
    printer: &Printer,
    id1: i64,
    id2: i64,
) -> anyhow::Result<()> {
    let state = open_state_store(cli.state_dir.as_deref(), cli.scope())?;
    let snap1 = state
        .get_compliance_snapshot(id1)?
        .ok_or_else(|| anyhow::anyhow!("snapshot #{} not found", id1))?;
    let snap2 = state
        .get_compliance_snapshot(id2)?
        .ok_or_else(|| anyhow::anyhow!("snapshot #{} not found", id2))?;

    let diff = compute_compliance_diff(&snap1, &snap2);
    printer.emit(build_compliance_diff_doc(
        id1,
        id2,
        &snap1,
        &snap2,
        &diff,
        printer.arrow(),
    ));
    Ok(())
}

/// Diff key for a compliance check — first available identifier, prefixed by category.
pub(super) fn check_key(c: &ComplianceCheck) -> String {
    let id = c
        .target
        .as_deref()
        .or(c.name.as_deref())
        .or(c.key.as_deref())
        .or(c.path.as_deref())
        .unwrap_or("(unknown)");
    format!("{}:{}", c.category, id)
}

#[derive(Debug)]
pub struct ComplianceDiff {
    pub added: Vec<ComplianceCheck>,
    pub removed: Vec<ComplianceCheck>,
    pub changed: Vec<ComplianceCheckChange>,
}

/// Compute added/removed/changed between two snapshots; deterministically sorted.
///
/// `check_key` is not unique within a single snapshot — e.g. two `file`
/// category checks (a permissions check and a "present" check) can share one
/// target, or `effective_files` can list the same target twice (profile +
/// module). Grouping by key into a `Vec` (rather than collapsing into a single
/// map entry) and pairing positionally within each key's group keeps every
/// check in either snapshot present in the diff exactly once: paired entries
/// are compared for a status change, and any surplus on one side falls out as
/// added/removed instead of being silently dropped.
pub fn compute_compliance_diff(
    snap1: &ComplianceSnapshot,
    snap2: &ComplianceSnapshot,
) -> ComplianceDiff {
    use std::collections::HashMap;

    let mut map1: HashMap<String, Vec<&ComplianceCheck>> = HashMap::new();
    for c in &snap1.checks {
        map1.entry(check_key(c)).or_default().push(c);
    }
    let mut map2: HashMap<String, Vec<&ComplianceCheck>> = HashMap::new();
    for c in &snap2.checks {
        map2.entry(check_key(c)).or_default().push(c);
    }

    let mut added: Vec<ComplianceCheck> = Vec::new();
    let mut removed: Vec<ComplianceCheck> = Vec::new();
    let mut changed: Vec<ComplianceCheckChange> = Vec::new();

    let empty: Vec<&ComplianceCheck> = Vec::new();
    let mut keys: Vec<&String> = map1.keys().chain(map2.keys()).collect();
    keys.sort();
    keys.dedup();

    for key in keys {
        let list1 = map1.get(key).unwrap_or(&empty);
        let list2 = map2.get(key).unwrap_or(&empty);
        let paired = list1.len().min(list2.len());

        for i in 0..paired {
            let check1 = list1[i];
            let check2 = list2[i];
            if check1.status != check2.status {
                changed.push(ComplianceCheckChange {
                    key: key.clone(),
                    old_status: format!("{:?}", check1.status),
                    new_status: format!("{:?}", check2.status),
                    detail: check2.detail.clone(),
                });
            }
        }
        for check2 in &list2[paired..] {
            added.push((*check2).clone());
        }
        for check1 in &list1[paired..] {
            removed.push((*check1).clone());
        }
    }

    added.sort_by_key(check_key);
    removed.sort_by_key(check_key);
    changed.sort_by(|a, b| a.key.cmp(&b.key));

    ComplianceDiff {
        added,
        removed,
        changed,
    }
}

/// Pure builder: compliance diff Doc.
pub fn build_compliance_diff_doc(
    id1: i64,
    id2: i64,
    snap1: &ComplianceSnapshot,
    snap2: &ComplianceSnapshot,
    diff: &ComplianceDiff,
    arrow: &str,
) -> Doc {
    let mut doc = Doc::new()
        .heading(format!("Compliance Diff #{id1} {arrow} #{id2}"))
        .kv_block([
            ("Snapshot 1", snap1.timestamp.clone()),
            ("Snapshot 2", snap2.timestamp.clone()),
        ]);

    if diff.added.is_empty() && diff.removed.is_empty() && diff.changed.is_empty() {
        doc = doc.status(Role::Ok, "No differences between snapshots");
    } else {
        doc = doc.section_if_nonempty("Added", &diff.added, |s, items| {
            items.iter().fold(s, |s, c| s.bullet(check_key(c)))
        });
        doc = doc.section_if_nonempty("Removed", &diff.removed, |s, items| {
            items.iter().fold(s, |s, c| s.bullet(check_key(c)))
        });
        doc = doc.section_if_nonempty("Changed", &diff.changed, |s, items| {
            items.iter().fold(s, |s, c| {
                let role = match c.new_status.as_str() {
                    "Violation" => Role::Fail,
                    "Warning" => Role::Warn,
                    _ => Role::Ok,
                };
                s.status_with(
                    role,
                    format!("{} ({} {arrow} {})", c.key, c.old_status, c.new_status),
                    |sf| sf.detail_opt(c.detail.as_deref()),
                )
            })
        });
        // The per-section titles carry no count, and unlike the module-review
        // sections above them a diff between two large snapshots can scroll
        // past the screen — this is the surface where the total IS the
        // headline, so it closes with one rather than making the reader
        // count rows.
        doc = doc.status_with(Role::Info, "Compliance diff", |f| {
            f.detail(format!(
                "{} added, {} removed, {} changed",
                diff.added.len(),
                diff.removed.len(),
                diff.changed.len()
            ))
        });
    }

    doc.with_data(ComplianceDiffOutput {
        id1,
        id2,
        added: diff.added.clone(),
        removed: diff.removed.clone(),
        changed: diff.changed.clone(),
    })
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
            s.status_with(Role::Fail, check_key(c), |sf| {
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
            s.status_with(Role::Warn, check_key(c), |sf| {
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
        format!(
            "All {} compliant",
            cfgd_core::pluralize(snapshot.summary.compliant, "check")
        )
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
            format!("Compliance snapshot written to {}", export_path.posix()),
        )
        .section("Summary", |s| {
            s.kv("Compliant", snapshot.summary.compliant.to_string())
                .kv("Warning", snapshot.summary.warning.to_string())
                .kv("Violation", snapshot.summary.violation.to_string())
        })
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
        let mut table = Table::new(["ID", "Timestamp", "Compliant", "Warning", "Violation"]);
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
            config_explicit: false,
            profile: None,
            verbose: 0,
            quiet: true,
            no_color: true,
            color: crate::cli::ColorWhen::Auto,
            output: OutputFormatArg(OutputFormat::Table),
            list_envelope: false,
            jsonpath: None,
            state_dir: Some(state_dir.to_path_buf()),
            config_dir: None,
            cache_dir: None,
            runtime_dir: None,
            scope_arg: crate::cli::ScopeArg::User,
            command: None,
        }
    }

    fn store_snapshot(state_dir: &std::path::Path, snapshot: &ComplianceSnapshot) {
        let state = open_state_store(Some(state_dir), cfgd_core::Scope::User).unwrap();
        state.store_compliance_snapshot(snapshot).unwrap();
    }

    // --- build_compliance_summary_doc ---

    #[test]
    fn build_compliance_summary_doc_all_compliant() {
        let snapshot = sample_snapshot(vec![
            check("file", "/etc/hosts", ComplianceStatus::Compliant),
            check("package", "ripgrep", ComplianceStatus::Compliant),
        ]);
        let (printer, cap) = Printer::for_test_doc();
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
            output.contains("All 2 checks compliant"),
            "should print all-compliant summary, got: {output}"
        );
    }

    #[test]
    fn build_compliance_summary_doc_warning_route() {
        let snapshot = sample_snapshot(vec![
            check("file", "/etc/a", ComplianceStatus::Compliant),
            check("system", "sysctl.x", ComplianceStatus::Warning),
        ]);
        let (printer, cap) = Printer::for_test_doc();
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
        let (printer, cap) = Printer::for_test_doc();
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
        let (printer, cap) = Printer::for_test_doc();
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

    #[test]
    fn cmd_compliance_diff_missing_snapshots_returns_err() {
        let state_dir = tempfile::tempdir().unwrap();
        let cli = test_cli_for(state_dir.path());
        let (printer, _cap) = Printer::for_test_doc();

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
        let (printer, cap) = Printer::for_test_doc();

        cmd_compliance_diff(&cli, &printer, 1, 2).unwrap();
        drop(printer);

        let output = cap.human();
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
        let (printer, cap) = Printer::for_test_doc();

        cmd_compliance_diff(&cli, &printer, 1, 2).unwrap();
        drop(printer);

        let output = cap.human();
        assert!(
            output.contains("Added") && output.contains("file:/c"),
            "should report added check file:/c, got: {output}"
        );
        assert!(
            output.contains("Removed") && output.contains("file:/b"),
            "should report removed check file:/b, got: {output}"
        );
        assert!(
            output.contains("Changed") && output.contains("file:/a"),
            "should report changed check file:/a, got: {output}"
        );
        assert!(
            output.contains("Compliant") && output.contains("Violation"),
            "changed line should include old + new status, got: {output}"
        );
    }

    // --- compute_compliance_diff: duplicate check_key within one snapshot ---

    #[test]
    fn compute_compliance_diff_pairs_duplicate_keys_instead_of_collapsing() {
        // Two checks share `file:/a` in each snapshot (e.g. `effective_files`
        // listing the same target twice via profile + module). A HashMap
        // collapse keyed on `check_key` would silently drop every check but
        // the last inserted per key before a diff is ever computed — this
        // pins that the first-in-snap1/first-in-snap2 pair is compared too,
        // not just whichever pair a map's last-write-wins happened to keep.
        let snap1 = sample_snapshot(vec![
            check("file", "/a", ComplianceStatus::Compliant),
            check("file", "/a", ComplianceStatus::Compliant),
        ]);
        let snap2 = sample_snapshot(vec![
            check("file", "/a", ComplianceStatus::Violation),
            check("file", "/a", ComplianceStatus::Compliant),
        ]);

        let diff = compute_compliance_diff(&snap1, &snap2);

        assert!(diff.added.is_empty(), "no surplus checks: {diff:?}");
        assert!(diff.removed.is_empty(), "no surplus checks: {diff:?}");
        assert_eq!(
            diff.changed.len(),
            1,
            "the first pair (Compliant -> Violation) must be reported; the \
             second pair (Compliant -> Compliant) is unchanged: {diff:?}"
        );
        assert_eq!(diff.changed[0].key, "file:/a");
        assert_eq!(diff.changed[0].new_status, "Violation");
    }

    #[test]
    fn compute_compliance_diff_reports_duplicate_key_surplus_as_added() {
        // snap2 carries one extra check under a key snap1 has only once. The
        // shared instance pairs and compares; the surplus must surface as
        // `added`, not vanish because a map already had that key occupied.
        let snap1 = sample_snapshot(vec![check("file", "/a", ComplianceStatus::Compliant)]);
        let snap2 = sample_snapshot(vec![
            check("file", "/a", ComplianceStatus::Compliant),
            check("file", "/a", ComplianceStatus::Violation),
        ]);

        let diff = compute_compliance_diff(&snap1, &snap2);

        assert!(
            diff.changed.is_empty(),
            "the shared pair is unchanged: {diff:?}"
        );
        assert!(diff.removed.is_empty(), "{diff:?}");
        assert_eq!(
            diff.added.len(),
            1,
            "the surplus check must be added, not dropped: {diff:?}"
        );
        assert_eq!(diff.added[0].status, ComplianceStatus::Violation);
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

        let cli = test_cli_for(state_dir.path());
        let (printer, cap) = Printer::for_test_doc();

        cmd_compliance_diff(&cli, &printer, 1, 2).unwrap();
        drop(printer);

        let parsed = cap.json().expect("diff Doc carries with_data payload");
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
        let (printer, cap) = Printer::for_test_doc();

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
        let (printer, _cap) = Printer::for_test_doc();

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
        let (printer, cap) = Printer::for_test_doc();

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
        let (printer, cap) = Printer::for_test_doc();

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

    // --- append_constraint_violation_checks ---

    #[test]
    fn append_constraint_violation_checks_adds_violation_and_bumps_summary() {
        use cfgd_core::composition::ConstraintViolation;

        let mut snapshot = sample_snapshot(vec![check(
            "file",
            "/etc/hosts",
            ComplianceStatus::Compliant,
        )]);
        let before = snapshot.summary.violation;

        let violations = vec![ConstraintViolation {
            source_name: "ec-source-repo".into(),
            path: Some("/home/u/.config/secret-unprotected.yaml".into()),
            kind: "encryption-required".into(),
            detail: "file '/home/u/.config/secret-unprotected.yaml' matches required-encryption \
                     target 'secret*' in source 'ec-source-repo' but has no encryption block"
                .into(),
        }];

        append_constraint_violation_checks(&mut snapshot, &violations);

        // An encryption-required violation lands in the file-encryption category.
        let added = snapshot
            .checks
            .iter()
            .find(|c| c.category == "file-encryption")
            .expect("encryption-required violation must be a file-encryption check");
        assert_eq!(added.status, ComplianceStatus::Violation);
        assert_eq!(
            added.target.as_deref(),
            Some("/home/u/.config/secret-unprotected.yaml")
        );
        assert!(
            added
                .detail
                .as_deref()
                .unwrap()
                .contains("no encryption block"),
            "detail must carry the verbatim constraint message"
        );
        assert_eq!(
            snapshot.summary.violation,
            before + 1,
            "summary.violation must bump by the appended violation"
        );
    }

    #[test]
    fn append_constraint_violation_checks_noop_when_empty() {
        let mut snapshot =
            sample_snapshot(vec![check("file", "/etc/a", ComplianceStatus::Compliant)]);
        let n = snapshot.checks.len();
        append_constraint_violation_checks(&mut snapshot, &[]);
        assert_eq!(
            snapshot.checks.len(),
            n,
            "empty violations must not add checks"
        );
    }
}
