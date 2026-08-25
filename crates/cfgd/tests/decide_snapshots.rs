//! Snapshot tests for `cfgd decide`.
//!
//! Goldens live under `tests/output_snapshots/decide/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test decide_snapshots
//!
//! The live `cmd_decide` reads from the SQLite state store; to keep snapshots
//! stable across hosts these tests drive the pure `build_decide_*_doc` helpers
//! with hand-crafted fixtures.

use std::path::Path;

use cfgd::cli::decide::{build_decide_bulk_doc, build_decide_list_doc};
use cfgd_core::output::Printer;
use cfgd_core::state::PendingDecision;

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

fn pending(
    source: &str,
    resource: &str,
    tier: &str,
    action: &str,
    summary: &str,
) -> PendingDecision {
    PendingDecision {
        id: 1,
        source: source.into(),
        resource: resource.into(),
        tier: tier.into(),
        action: action.into(),
        summary: summary.into(),
        created_at: "2026-05-11T00:00:00Z".into(),
        resolved_at: None,
        resolution: None,
        content_hash: None,
    }
}

fn pending_fixture() -> Vec<PendingDecision> {
    vec![
        pending(
            "team-config",
            "packages.brew.curl",
            "recommended",
            "install",
            "Install curl via brew",
        ),
        pending("team-config", "env.EDITOR", "optional", "set", "Set EDITOR"),
    ]
}

#[test]
fn decide_pending_human() {
    let decisions = pending_fixture();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_decide_list_doc(
        &decisions,
        &[],
        None,
        &Default::default(),
    ));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "decide/pending.txt");
}

#[test]
fn decide_pending_json() {
    let decisions = pending_fixture();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_decide_list_doc(
        &decisions,
        &[],
        None,
        &Default::default(),
    ));
    drop(printer);

    let actual = cap.json().expect("doc captured json");
    let decisions_json = actual
        .get("decisions")
        .expect("payload must expose `decisions` array");
    assert_eq!(
        decisions_json.as_array().map(|a| a.len()),
        Some(2),
        "decisions array must round-trip 2 items, got: {actual:?}"
    );
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "decide/pending.json");
}

#[test]
fn decide_empty_human() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_decide_list_doc(&[], &[], None, &Default::default()));
    drop(printer);
    let human = cap.human();
    assert!(
        human.contains("No pending decisions"),
        "empty listing must include info status, got:\n{human}"
    );
    assert!(
        !human.contains("Pending Decisions"),
        "empty listing must omit the Pending Decisions section header, got:\n{human}"
    );
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "decide/empty.txt");
}

#[test]
fn decide_pending_multi_source_human() {
    // BTreeMap-driven grouping pins alphabetical source order regardless of
    // insertion order. Insert team-config → org-config → app-config; expect
    // app-config → org-config → team-config in the rendered output.
    let decisions = vec![
        pending(
            "team-config",
            "packages.brew.curl",
            "recommended",
            "install",
            "Install curl",
        ),
        pending("org-config", "env.EDITOR", "optional", "set", "Set EDITOR"),
        pending(
            "app-config",
            "file/bashrc",
            "required",
            "create",
            "Create bashrc",
        ),
    ];
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_decide_list_doc(
        &decisions,
        &[],
        None,
        &Default::default(),
    ));
    drop(printer);
    let human = cap.human();
    let app = human
        .find("source:app-config")
        .expect("app-config subsection");
    let org = human
        .find("source:org-config")
        .expect("org-config subsection");
    let team = human
        .find("source:team-config")
        .expect("team-config subsection");
    assert!(
        app < org && org < team,
        "expected app-config < org-config < team-config in:\n{human}"
    );
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "decide/pending_multi_source.txt");
}

#[test]
fn decide_pending_single_item_human() {
    // Singular `1 pending item` (no trailing 's') for exactly one item per source.
    let decisions = vec![pending(
        "solo-source",
        "file/bashrc",
        "required",
        "create",
        "Create bashrc",
    )];
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_decide_list_doc(
        &decisions,
        &[],
        None,
        &Default::default(),
    ));
    drop(printer);
    let human = cap.human();
    assert!(
        human.contains("source:solo-source"),
        "expected the source owner token as the heading, got:\n{human}"
    );
    assert!(
        human.contains(&cfgd_core::reconciler::pending_decisions_title(1)),
        "the count is the section's own annotation, singular for one, got:\n{human}"
    );
    assert!(
        !human.contains("1 items"),
        "must not pluralize for count=1, got:\n{human}"
    );
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "decide/pending_single_item.txt");
}

#[test]
fn decide_after_accept_human() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_decide_bulk_doc("accepted", 2, None));
    drop(printer);
    let human = cap.human();
    assert!(
        human.contains("Accepted 2 items"),
        "bulk accept summary reads as a sentence: past-tense verb, then the count, got:\n{human}"
    );
    assert!(
        human.contains("next reconcile"),
        "bulk accept must hint about next reconcile, got:\n{human}"
    );
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "decide/after_accept.txt");
}

/// The row says WHAT is being decided, not a restatement of its own
/// coordinates: the content the source would put on the machine, recovered
/// from the profile it delivered.
#[test]
fn decide_pending_names_the_content_of_each_item() {
    let config_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(config_dir.path().join("files")).unwrap();
    std::fs::write(
        config_dir.path().join("files").join("bashrc"),
        "line one\nline two\nline three\n",
    )
    .unwrap();

    let spec: cfgd_core::config::ProfileSpec = serde_yaml::from_str(
        "env:\n  - name: EDITOR\n    value: vim\npackages:\n  brew:\n    - curl\nfiles:\n  managed:\n    - source: files/bashrc\n      target: /home/u/.bashrc\n      permissions: \"0644\"\nsystem:\n  shellAliases:\n    gs: git status\n",
    )
    .unwrap();
    let layers = vec![cfgd_core::config::ProfileLayer {
        source: "team-config".to_string(),
        profile_name: "team".to_string(),
        priority: 500,
        policy: cfgd_core::config::LayerPolicy::Recommended,
        spec,
    }];
    let merged = cfgd_core::config::merge_layers(&layers);
    let resolved = cfgd_core::config::ResolvedProfile { layers, merged };

    let decisions = vec![
        pending(
            "team-config",
            "env.EDITOR",
            "recommended",
            "install",
            "recommended env.EDITOR (from team-config)",
        ),
        pending(
            "team-config",
            "packages.brew.curl",
            "recommended",
            "install",
            "recommended packages.brew.curl (from team-config) — installed 7.1, source wants ^8",
        ),
        pending(
            "team-config",
            "files./home/u/.bashrc",
            "required",
            "install",
            "required files./home/u/.bashrc (from team-config)",
        ),
        pending(
            "team-config",
            "system.shellAliases",
            "recommended",
            "install",
            "recommended system.shellAliases (from team-config)",
        ),
        // Nothing in the delivered profile declares it any more, so the row
        // stands as its subject alone: the stored summary is a persisted
        // string no display row reads.
        pending(
            "team-config",
            "env.GONE",
            "optional",
            "install",
            "optional env.GONE (from team-config)",
        ),
    ];
    let contents = cfgd_core::reconciler::DecisionContents::for_decisions(
        &resolved,
        &decisions,
        config_dir.path(),
        &resolved.merged.entry_owners,
    );

    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_decide_list_doc(&decisions, &[], None, &contents));
    drop(printer);
    let human = cap.human();
    // The subject column is padded by the renderer, so each row is asserted
    // as its two halves rather than as one joined literal.
    assert!(
        human.contains("Recommended env.EDITOR") && human.contains("— EDITOR=vim"),
        "env row must name the value: {human}"
    );
    assert!(
        human.contains("— brew install curl — installed 7.1, source wants ^8"),
        "package row must name the install and keep its conflict annotation: {human}"
    );
    assert!(
        human.contains("— 3 lines, mode 0644"),
        "file row must name size and mode, never the body: {human}"
    );
    assert!(
        human.contains("— {\"gs\":\"git status\"}"),
        "a structured system setting renders on one line: {human}"
    );
    assert!(
        !human.contains("line one"),
        "a file's body is never rendered into a decision row: {human}"
    );
    assert!(
        human.contains("Optional env.GONE") && !human.contains("(from team-config)"),
        "an unrecoverable item renders its subject alone, never the stored summary: {human}"
    );
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "decide/pending_content.txt");
}
