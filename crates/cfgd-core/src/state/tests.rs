use rusqlite::params;

use super::*;

#[test]
fn open_in_memory() {
    let store = StateStore::open_in_memory().unwrap();
    assert!(store.last_apply().unwrap().is_none());
}

#[test]
fn record_and_retrieve_apply() {
    let store = StateStore::open_in_memory().unwrap();
    let id = store
        .record_apply(
            "default",
            "abc123",
            ApplyStatus::Success,
            Some("{\"files\": 3}"),
        )
        .unwrap();
    assert!(id > 0);

    let last = store.last_apply().unwrap().unwrap();
    assert_eq!(last.id, id);
    assert_eq!(last.profile, "default");
    assert_eq!(last.plan_hash, "abc123");
    assert_eq!(last.status, ApplyStatus::Success);
    assert_eq!(last.summary.as_deref(), Some("{\"files\": 3}"));
}

#[test]
fn history_returns_most_recent_first() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .record_apply("p1", "h1", ApplyStatus::Success, None)
        .unwrap();
    store
        .record_apply("p2", "h2", ApplyStatus::Partial, None)
        .unwrap();
    store
        .record_apply("p3", "h3", ApplyStatus::Failed, None)
        .unwrap();

    let history = store.history(10).unwrap();
    assert_eq!(history.len(), 3);
    assert_eq!(history[0].profile, "p3");
    assert_eq!(history[1].profile, "p2");
    assert_eq!(history[2].profile, "p1");
}

#[test]
fn history_respects_limit() {
    let store = StateStore::open_in_memory().unwrap();
    for i in 0..10 {
        store
            .record_apply(&format!("p{}", i), "h", ApplyStatus::Success, None)
            .unwrap();
    }

    let history = store.history(3).unwrap();
    assert_eq!(history.len(), 3);
}

#[test]
fn record_and_retrieve_drift() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .record_drift(
            "file",
            "/home/user/.zshrc",
            Some("abc"),
            Some("def"),
            "local",
        )
        .unwrap();

    let events = store.unresolved_drift().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].resource_type, "file");
    assert_eq!(events[0].resource_id, "/home/user/.zshrc");
    assert_eq!(events[0].expected.as_deref(), Some("abc"));
    assert_eq!(events[0].actual.as_deref(), Some("def"));
    assert!(events[0].resolved_by.is_none());
}

#[test]
fn resolve_drift_links_to_apply() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .record_drift("file", "/test", Some("a"), Some("b"), "local")
        .unwrap();

    let apply_id = store
        .record_apply("default", "h", ApplyStatus::Success, None)
        .unwrap();
    store.resolve_drift(apply_id, "file", "/test").unwrap();

    let events = store.unresolved_drift().unwrap();
    assert!(events.is_empty());
}

#[test]
fn record_drift_upserts_same_resource() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .record_drift("file", "/etc/hosts", None, Some("drift detected"), "local")
        .unwrap();
    store
        .record_drift("file", "/etc/hosts", None, Some("drift detected"), "local")
        .unwrap();

    let events = store.unresolved_drift().unwrap();
    assert_eq!(
        events.len(),
        1,
        "recording the same resource twice must yield ONE unresolved row, not two"
    );
    assert_eq!(events[0].resource_type, "file");
    assert_eq!(events[0].resource_id, "/etc/hosts");
}

#[test]
fn record_drift_distinct_resources_stay_separate() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .record_drift("file", "/etc/hosts", None, Some("x"), "local")
        .unwrap();
    store
        .record_drift("file", "/etc/resolv.conf", None, Some("x"), "local")
        .unwrap();

    let events = store.unresolved_drift().unwrap();
    assert_eq!(events.len(), 2, "distinct resources keep distinct rows");
}

#[test]
fn resolve_drift_not_in_resolves_complement() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .record_drift("file", "/keep", None, Some("x"), "local")
        .unwrap();
    store
        .record_drift("file", "/heal", None, Some("x"), "local")
        .unwrap();

    // Only /keep is still drifting this tick; /heal healed.
    let current = vec![("file".to_string(), "/keep".to_string())];
    store.resolve_drift_not_in(&current).unwrap();

    let events = store.unresolved_drift().unwrap();
    assert_eq!(events.len(), 1, "the healed row must be resolved");
    assert_eq!(events[0].resource_id, "/keep");
}

#[test]
fn resolve_drift_not_in_matches_on_full_tuple_not_id_alone() {
    // Two rows share resource_id "/etc/x" but differ by resource_type. Keeping
    // only the (file, /etc/x) pair must resolve the (secret, /etc/x) row — the
    // composite-key match must not treat the shared id as "still drifting".
    let store = StateStore::open_in_memory().unwrap();
    store
        .record_drift("file", "/etc/x", None, Some("x"), "local")
        .unwrap();
    store
        .record_drift("secret", "/etc/x", None, Some("x"), "local")
        .unwrap();

    let current = vec![("file".to_string(), "/etc/x".to_string())];
    store.resolve_drift_not_in(&current).unwrap();

    let events = store.unresolved_drift().unwrap();
    assert_eq!(events.len(), 1, "only the matching tuple survives");
    assert_eq!(events[0].resource_type, "file");
    assert_eq!(events[0].resource_id, "/etc/x");
}

#[test]
fn resolve_all_drift_clears_everything() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .record_drift("file", "/a", None, Some("x"), "local")
        .unwrap();
    store
        .record_drift("file", "/b", None, Some("x"), "local")
        .unwrap();

    store.resolve_all_drift().unwrap();

    assert!(
        store.unresolved_drift().unwrap().is_empty(),
        "resolve_all_drift must clear every unresolved row"
    );
}

#[test]
fn snapshot_reset_resolves_healed_resource() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .record_drift("file", "/x", None, Some("drift detected"), "local")
        .unwrap();
    assert_eq!(store.unresolved_drift().unwrap().len(), 1);

    // Next reconcile tick: X no longer drifts (current set is empty).
    let current: Vec<(String, String)> = Vec::new();
    store.resolve_drift_not_in(&current).unwrap();

    assert!(
        store.unresolved_drift().unwrap().is_empty(),
        "a clean reconcile snapshot must drive the unresolved count back to 0"
    );
}

#[test]
fn upsert_managed_resource() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .upsert_managed_resource("file", "/home/.zshrc", "local", Some("hash1"), None)
        .unwrap();

    let resources = store.managed_resources().unwrap();
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0].resource_type, "file");
    assert_eq!(resources[0].resource_id, "/home/.zshrc");
    assert_eq!(resources[0].last_hash.as_deref(), Some("hash1"));

    // Update with new hash
    store
        .upsert_managed_resource("file", "/home/.zshrc", "local", Some("hash2"), None)
        .unwrap();

    let resources = store.managed_resources().unwrap();
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0].last_hash.as_deref(), Some("hash2"));
}

#[test]
fn upsert_package_resource_persists_uninstall_cmd() {
    use crate::providers::OrphanedPackage;

    let store = StateStore::open_in_memory().unwrap();
    // Scripted manager package — carries a persisted uninstall command.
    store
        .upsert_package_resource(
            "widgetmgr/widget",
            "local",
            None,
            Some("widgetmgr rm {package}"),
        )
        .unwrap();
    // Built-in package — no persisted command (NULL).
    store
        .upsert_package_resource("cargo/foo", "local", None, None)
        .unwrap();

    let known: std::collections::HashSet<String> = ["cargo".to_string(), "apt".to_string()]
        .into_iter()
        .collect();
    let orphans = store.orphaned_package_resources(&known).unwrap();
    assert_eq!(
        orphans,
        vec![OrphanedPackage {
            manager: "widgetmgr".to_string(),
            package: "widget".to_string(),
            uninstall_cmd: Some("widgetmgr rm {package}".to_string()),
        }],
        "only the package whose manager left the known set is orphaned"
    );

    // The package row must still be queryable as a managed package.
    let ids = store.managed_package_ids().unwrap();
    assert!(ids.contains(&("cargo".to_string(), "foo".to_string())));
    assert!(ids.contains(&("widgetmgr".to_string(), "widget".to_string())));
}

#[test]
fn upsert_package_resource_refreshes_changed_uninstall_cmd() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .upsert_package_resource("widgetmgr/widget", "local", None, Some("old rm {package}"))
        .unwrap();
    // Re-install with a changed script must update the persisted command.
    store
        .upsert_package_resource("widgetmgr/widget", "local", None, Some("new rm {package}"))
        .unwrap();

    let known = std::collections::HashSet::new();
    let orphans = store.orphaned_package_resources(&known).unwrap();
    assert_eq!(orphans.len(), 1);
    assert_eq!(
        orphans[0].uninstall_cmd.as_deref(),
        Some("new rm {package}"),
        "re-install must refresh a changed uninstall script"
    );
}

#[test]
fn orphaned_package_resources_empty_when_manager_known() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .upsert_package_resource("widgetmgr/widget", "local", None, Some("widgetmgr rm"))
        .unwrap();

    let known: std::collections::HashSet<String> = ["widgetmgr".to_string()].into_iter().collect();
    let orphans = store.orphaned_package_resources(&known).unwrap();
    assert!(
        orphans.is_empty(),
        "a package whose manager is still in the registry is not orphaned"
    );
}

#[test]
fn orphaned_package_resources_reports_null_cmd_rows() {
    let store = StateStore::open_in_memory().unwrap();
    // A custom-manager package tracked before the persisted-uninstall column
    // existed: NULL command, but still orphaned and must be reported.
    store
        .upsert_package_resource("legacymgr/legacypkg", "local", None, None)
        .unwrap();

    let known = std::collections::HashSet::new();
    let orphans = store.orphaned_package_resources(&known).unwrap();
    assert_eq!(orphans.len(), 1);
    assert_eq!(orphans[0].manager, "legacymgr");
    assert_eq!(orphans[0].package, "legacypkg");
    assert!(
        orphans[0].uninstall_cmd.is_none(),
        "a NULL persisted command must surface as None for the caller to warn on"
    );
}

#[test]
fn generic_upsert_managed_resource_leaves_uninstall_cmd_null() {
    let store = StateStore::open_in_memory().unwrap();
    // The generic upsert (used for files/system resources) must not touch the
    // new column — it stays NULL.
    store
        .upsert_managed_resource("package", "widgetmgr/widget", "local", None, None)
        .unwrap();
    let known = std::collections::HashSet::new();
    let orphans = store.orphaned_package_resources(&known).unwrap();
    assert_eq!(orphans.len(), 1);
    assert!(orphans[0].uninstall_cmd.is_none());
}

#[test]
fn is_resource_managed() {
    let store = StateStore::open_in_memory().unwrap();

    assert!(!store.is_resource_managed("file", "/home/.zshrc").unwrap());

    store
        .upsert_managed_resource("file", "/home/.zshrc", "local", Some("hash1"), None)
        .unwrap();

    assert!(store.is_resource_managed("file", "/home/.zshrc").unwrap());
    assert!(!store.is_resource_managed("file", "/home/.bashrc").unwrap());
    assert!(
        !store
            .is_resource_managed("package", "/home/.zshrc")
            .unwrap()
    );
}

#[test]
fn remove_managed_resource_deletes_tracked_row() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .upsert_managed_resource("package", "apt/fd-find", "local", None, None)
        .unwrap();
    assert!(store.is_resource_managed("package", "apt/fd-find").unwrap());

    store
        .remove_managed_resource("package", "apt/fd-find")
        .unwrap();
    assert!(!store.is_resource_managed("package", "apt/fd-find").unwrap());
}

#[test]
fn remove_managed_resource_is_idempotent_on_missing_row() {
    let store = StateStore::open_in_memory().unwrap();
    // Deleting a row that was never tracked is a no-op, not an error.
    store
        .remove_managed_resource("package", "apt/nonexistent")
        .unwrap();
}

#[test]
fn managed_package_ids_round_trip() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .upsert_managed_resource("package", "apt/fd-find", "local", None, None)
        .unwrap();
    store
        .upsert_managed_resource("package", "cargo/ripgrep", "local", None, None)
        .unwrap();
    // A non-package resource must never appear in the package id list.
    store
        .upsert_managed_resource("file", "/home/.zshrc", "local", None, None)
        .unwrap();

    let mut ids = store.managed_package_ids().unwrap();
    ids.sort();
    assert_eq!(
        ids,
        vec![
            ("apt".to_string(), "fd-find".to_string()),
            ("cargo".to_string(), "ripgrep".to_string()),
        ]
    );

    store
        .remove_managed_resource("package", "apt/fd-find")
        .unwrap();
    let ids = store.managed_package_ids().unwrap();
    assert_eq!(ids, vec![("cargo".to_string(), "ripgrep".to_string())]);
}

#[test]
fn managed_resources_unique_constraint() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .upsert_managed_resource("file", "/a", "local", None, None)
        .unwrap();
    store
        .upsert_managed_resource("package", "/a", "local", None, None)
        .unwrap();

    let resources = store.managed_resources().unwrap();
    assert_eq!(resources.len(), 2);
}

#[test]
fn plan_hash_is_deterministic() {
    let h1 = plan_hash("test plan data");
    let h2 = plan_hash("test plan data");
    assert_eq!(h1, h2);
    assert_ne!(h1, plan_hash("different data"));
}

#[test]
fn now_iso8601_format() {
    let ts = crate::utc_now_iso8601();
    assert!(ts.contains('T'));
    assert!(ts.ends_with('Z'));
    assert_eq!(ts.len(), 20);
}

#[test]
fn open_file_based_store() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.db");

    let store = StateStore::open(&db_path).unwrap();
    store
        .record_apply("test", "hash", ApplyStatus::Success, None)
        .unwrap();

    // Reopen and verify persistence
    let store2 = StateStore::open(&db_path).unwrap();
    let last = store2.last_apply().unwrap().unwrap();
    assert_eq!(last.profile, "test");
}

#[cfg(unix)]
#[test]
fn open_in_dir_readonly_dir_yields_directory_not_writable_naming_path() {
    use std::os::unix::fs::PermissionsExt;

    // Skip under root: a 0o500 dir is still writable to uid 0, so the probe
    // (correctly) reports it writable and this case cannot be exercised.
    if crate::is_root() {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir(&state_dir).unwrap();
    std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o500)).unwrap();

    let err = match StateStore::open_in_dir(&state_dir) {
        Ok(_) => panic!("read-only state dir must error"),
        Err(e) => e,
    };
    match &err {
        crate::errors::CfgdError::State(StateError::DirectoryNotWritable { path }) => {
            assert_eq!(path, &state_dir, "error must name the unwritable state dir");
        }
        other => panic!("expected DirectoryNotWritable, got: {other}"),
    }
    assert!(
        err.to_string().contains(&state_dir.display().to_string()),
        "rendered error must name the path: {err}"
    );

    // Calling twice yields the same typed error — no partial DB / crash loop.
    let err2 = match StateStore::open_in_dir(&state_dir) {
        Ok(_) => panic!("second open also errors"),
        Err(e) => e,
    };
    assert!(matches!(
        &err2,
        crate::errors::CfgdError::State(StateError::DirectoryNotWritable { .. })
    ));

    // Restore perms so tempdir cleanup can remove the directory.
    std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
}

// --- Config source state tests ---

#[test]
fn upsert_and_list_config_sources() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .upsert_config_source(
            "acme",
            "git@github.com:acme/config.git",
            "master",
            Some("abc123"),
            Some("2.1.0"),
            Some("~2"),
        )
        .unwrap();

    let sources = store.config_sources().unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].name, "acme");
    assert_eq!(sources[0].origin_url, "git@github.com:acme/config.git");
    assert_eq!(sources[0].last_commit.as_deref(), Some("abc123"));
    assert_eq!(sources[0].source_version.as_deref(), Some("2.1.0"));
    assert_eq!(sources[0].status, "active");
}

#[test]
fn config_source_by_name() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .upsert_config_source("acme", "url", "main", None, None, None)
        .unwrap();

    let found = store.config_source_by_name("acme").unwrap();
    assert!(found.is_some());
    assert_eq!(found.unwrap().name, "acme");

    let not_found = store.config_source_by_name("nonexistent").unwrap();
    assert!(not_found.is_none());
}

#[test]
fn remove_config_source() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .upsert_config_source("acme", "url", "main", None, None, None)
        .unwrap();

    store.remove_config_source("acme").unwrap();
    let sources = store.config_sources().unwrap();
    assert!(sources.is_empty());
}

#[test]
fn update_config_source_status() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .upsert_config_source("acme", "url", "main", None, None, None)
        .unwrap();

    store
        .update_config_source_status("acme", "inactive")
        .unwrap();
    let source = store.config_source_by_name("acme").unwrap().unwrap();
    assert_eq!(source.status, "inactive");
}

#[test]
fn record_source_conflict() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .record_source_conflict(
            "acme",
            "package",
            "git-secrets (brew)",
            "REQUIRED",
            Some("team requirement"),
        )
        .unwrap();

    // Verify the conflict was actually persisted
    let count: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM source_conflicts WHERE source_name = ?1",
            params!["acme"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "one conflict should be recorded");

    let (resource_type, resource_id, resolution, detail): (String, String, String, Option<String>) = store
            .conn
            .query_row(
                "SELECT resource_type, resource_id, resolution, detail FROM source_conflicts WHERE source_name = ?1",
                params!["acme"],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
    assert_eq!(resource_type, "package");
    assert_eq!(resource_id, "git-secrets (brew)");
    assert_eq!(resolution, "REQUIRED");
    assert_eq!(detail.as_deref(), Some("team requirement"));
}

#[test]
fn managed_resources_by_source() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .upsert_managed_resource("file", "/a", "local", None, None)
        .unwrap();
    store
        .upsert_managed_resource("file", "/b", "acme", None, None)
        .unwrap();
    store
        .upsert_managed_resource("package", "git-secrets", "acme", None, None)
        .unwrap();

    let acme_resources = store.managed_resources_by_source("acme").unwrap();
    assert_eq!(acme_resources.len(), 2);

    let local_resources = store.managed_resources_by_source("local").unwrap();
    assert_eq!(local_resources.len(), 1);
}

#[test]
fn upsert_config_source_updates_on_conflict() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .upsert_config_source("acme", "url1", "main", Some("commit1"), Some("1.0.0"), None)
        .unwrap();
    store
        .upsert_config_source(
            "acme",
            "url2",
            "dev",
            Some("commit2"),
            Some("2.0.0"),
            Some("~2"),
        )
        .unwrap();

    let sources = store.config_sources().unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].origin_url, "url2");
    assert_eq!(sources[0].origin_branch, "dev");
    assert_eq!(sources[0].last_commit.as_deref(), Some("commit2"));
    assert_eq!(sources[0].source_version.as_deref(), Some("2.0.0"));
}

// --- Pending decision tests ---

/// The resource path of every withholding decision, in row order.
fn withheld_resources(store: &StateStore) -> Vec<String> {
    store
        .withheld_decisions()
        .expect("read withholding decisions")
        .into_iter()
        .map(|d| d.resource)
        .collect()
}

#[test]
fn withheld_paths_cover_both_states_that_keep_a_resource_off_the_machine() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .upsert_pending_decision("acme", "packages.brew.k9s", "recommended", "install", "k9s")
        .unwrap();
    store
        .upsert_pending_decision(
            "acme",
            "packages.brew.stern",
            "recommended",
            "install",
            "st",
        )
        .unwrap();
    store
        .upsert_pending_decision("acme", "files.~/.zshrc", "recommended", "install", "rc")
        .unwrap();
    store
        .resolve_decision("packages.brew.stern", "rejected")
        .unwrap();
    store
        .resolve_decision("files.~/.zshrc", "accepted")
        .unwrap();

    let mut withheld = withheld_resources(&store);
    withheld.sort();
    assert_eq!(
        withheld,
        vec![
            "packages.brew.k9s".to_string(),
            "packages.brew.stern".to_string()
        ],
        "awaiting and declined both withhold; only an accepted decision releases its resource"
    );
}

#[test]
fn accepting_a_resource_that_was_once_rejected_releases_it() {
    // A source update re-asks about an item the operator declined earlier
    // (`docs/sources.md`: "Rejection doesn't persist across source versions"),
    // so a resource can carry a resolved rejection AND a newer decision. The
    // newest answer is the one that counts — otherwise accepting the fresh
    // decision would be silently overruled by the stale rejection.
    let store = StateStore::open_in_memory().unwrap();
    store
        .upsert_pending_decision("acme", "packages.brew.k9s", "recommended", "install", "v1")
        .unwrap();
    store
        .resolve_decision("packages.brew.k9s", "rejected")
        .unwrap();
    store
        .upsert_pending_decision("acme", "packages.brew.k9s", "recommended", "install", "v2")
        .unwrap();
    assert_eq!(
        withheld_resources(&store),
        vec!["packages.brew.k9s".to_string()],
        "the fresh decision withholds while it is unanswered"
    );

    store
        .resolve_decision("packages.brew.k9s", "accepted")
        .unwrap();
    assert!(
        withheld_resources(&store).is_empty(),
        "the stale rejection must not outlive the answer that replaced it"
    );
}

#[test]
fn record_auto_accepted_resolves_the_open_row_with_auto_provenance() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .upsert_pending_decision(
            "acme",
            "packages.cargo.bat",
            "recommended",
            "install",
            "recommended packages.cargo.bat (from acme)",
        )
        .unwrap();
    store
        .record_auto_accepted_decision(
            "acme",
            "packages.cargo.bat",
            "recommended",
            "install",
            "recommended packages.cargo.bat (from acme) — auto-accepted: already installed",
        )
        .unwrap();

    assert!(
        withheld_resources(&store).is_empty(),
        "an auto-accepted resolution releases the resource"
    );
    let (resolution, resolved_at, summary): (Option<String>, Option<String>, String) = store
        .conn
        .query_row(
            "SELECT resolution, resolved_at, summary FROM pending_decisions
                 WHERE source = 'acme' AND resource = 'packages.cargo.bat'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        resolution.as_deref(),
        Some(super::RESOLUTION_AUTO_ACCEPTED),
        "distinguishable from an operator's `accepted`"
    );
    assert!(resolved_at.is_some());
    assert!(summary.contains("auto-accepted: already installed"));
}

#[test]
fn record_auto_accepted_with_no_open_row_inserts_once() {
    // No row existed (nothing had asked yet): the provenance still lands, as
    // an already-resolved row — and re-observing the same fact on the next
    // run is a no-op, not a history row per tick.
    let store = StateStore::open_in_memory().unwrap();
    let summary = "recommended packages.cargo.bat (from acme) — auto-accepted: already installed";
    for _ in 0..2 {
        store
            .record_auto_accepted_decision(
                "acme",
                "packages.cargo.bat",
                "recommended",
                "install",
                summary,
            )
            .unwrap();
    }

    let count: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM pending_decisions
                 WHERE source = 'acme' AND resource = 'packages.cargo.bat'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "idempotent for an unchanged observation");
    assert!(store.pending_decisions().unwrap().is_empty());
    assert!(withheld_resources(&store).is_empty());
    assert!(
        store.has_decision("acme", "packages.cargo.bat").unwrap(),
        "the provenance row exists for `status` to explain"
    );
}

#[test]
fn discarding_a_removed_source_leaves_no_lasting_exclusion() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .upsert_pending_decision("acme", "packages.brew.k9s", "recommended", "install", "k9s")
        .unwrap();
    store
        .upsert_pending_decision(
            "other",
            "packages.brew.bat",
            "recommended",
            "install",
            "bat",
        )
        .unwrap();

    assert_eq!(store.discard_decisions_for_source("acme").unwrap(), 1);
    assert_eq!(
        withheld_resources(&store),
        vec!["packages.brew.bat".to_string()],
        "an unsubscribed source stops withholding the paths it named"
    );
    assert_eq!(
        store.pending_decisions().unwrap().len(),
        1,
        "only the removed source's rows are gone"
    );
}

#[test]
fn upsert_and_list_pending_decisions() {
    let store = StateStore::open_in_memory().unwrap();
    let id = store
        .upsert_pending_decision(
            "acme",
            "packages.brew.k9s",
            "recommended",
            "install",
            "install k9s (recommended by acme)",
        )
        .unwrap();
    assert!(id > 0);

    let decisions = store.pending_decisions().unwrap();
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].source, "acme");
    assert_eq!(decisions[0].resource, "packages.brew.k9s");
    assert_eq!(decisions[0].tier, "recommended");
    assert_eq!(decisions[0].action, "install");
    assert!(decisions[0].resolved_at.is_none());
}

#[test]
fn upsert_pending_decision_updates_existing() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .upsert_pending_decision(
            "acme",
            "packages.brew.k9s",
            "recommended",
            "install",
            "original summary",
        )
        .unwrap();
    store
        .upsert_pending_decision(
            "acme",
            "packages.brew.k9s",
            "recommended",
            "update",
            "updated summary",
        )
        .unwrap();

    let decisions = store.pending_decisions().unwrap();
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].action, "update");
    assert_eq!(decisions[0].summary, "updated summary");
}

#[test]
fn resolve_decision_by_resource() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .upsert_pending_decision("acme", "packages.brew.k9s", "recommended", "install", "k9s")
        .unwrap();

    let resolved = store
        .resolve_decision("packages.brew.k9s", "accepted")
        .unwrap();
    assert!(resolved);

    let pending = store.pending_decisions().unwrap();
    assert!(pending.is_empty());
}

#[test]
fn resolve_decision_nonexistent_returns_false() {
    let store = StateStore::open_in_memory().unwrap();
    let resolved = store
        .resolve_decision("nonexistent.resource", "accepted")
        .unwrap();
    assert!(!resolved);
}

#[test]
fn resolve_decisions_for_source() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .upsert_pending_decision("acme", "packages.brew.k9s", "recommended", "install", "k9s")
        .unwrap();
    store
        .upsert_pending_decision(
            "acme",
            "packages.brew.stern",
            "recommended",
            "install",
            "stern",
        )
        .unwrap();
    store
        .upsert_pending_decision("other", "packages.brew.bat", "optional", "install", "bat")
        .unwrap();

    let count = store
        .resolve_decisions_for_source("acme", "accepted")
        .unwrap();
    assert_eq!(count, 2);

    let pending = store.pending_decisions().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].source, "other");
}

#[test]
fn resolve_all_decisions() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .upsert_pending_decision("a", "r1", "recommended", "install", "s1")
        .unwrap();
    store
        .upsert_pending_decision("b", "r2", "optional", "install", "s2")
        .unwrap();

    let count = store.resolve_all_decisions("accepted").unwrap();
    assert_eq!(count, 2);

    let pending = store.pending_decisions().unwrap();
    assert!(pending.is_empty());
}

#[test]
fn pending_decisions_for_source() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .upsert_pending_decision("acme", "r1", "recommended", "install", "s1")
        .unwrap();
    store
        .upsert_pending_decision("other", "r2", "optional", "install", "s2")
        .unwrap();

    let acme = store.pending_decisions_for_source("acme").unwrap();
    assert_eq!(acme.len(), 1);
    assert_eq!(acme[0].resource, "r1");
}

// --- Source config hash tests ---

#[test]
fn set_and_get_source_config_hash() {
    let store = StateStore::open_in_memory().unwrap();
    store.set_source_config_hash("acme", "hash123").unwrap();

    let hash = store.source_config_hash("acme").unwrap().unwrap();
    assert_eq!(hash.config_hash, "hash123");
}

#[test]
fn source_config_hash_upsert() {
    let store = StateStore::open_in_memory().unwrap();
    store.set_source_config_hash("acme", "hash1").unwrap();
    store.set_source_config_hash("acme", "hash2").unwrap();

    let hash = store.source_config_hash("acme").unwrap().unwrap();
    assert_eq!(hash.config_hash, "hash2");
}

#[test]
fn source_config_hash_not_found() {
    let store = StateStore::open_in_memory().unwrap();
    let hash = store.source_config_hash("nonexistent").unwrap();
    assert!(hash.is_none());
}

#[test]
fn remove_source_config_hash() {
    let store = StateStore::open_in_memory().unwrap();
    store.set_source_config_hash("acme", "hash1").unwrap();
    store.remove_source_config_hash("acme").unwrap();

    let hash = store.source_config_hash("acme").unwrap();
    assert!(hash.is_none());
}

#[test]
fn file_backup_store_and_retrieve() {
    let store = StateStore::open_in_memory().unwrap();
    let apply_id = store
        .record_apply("test", "hash", ApplyStatus::Success, None)
        .unwrap();

    let state = crate::FileState {
        content: b"original content".to_vec(),
        content_hash: "abc123".to_string(),
        permissions: Some(0o644),
        is_symlink: false,
        symlink_target: None,
        oversized: false,
    };

    store
        .store_file_backup(apply_id, "/home/user/.bashrc", &state)
        .unwrap();

    let backup = store
        .get_file_backup(apply_id, "/home/user/.bashrc")
        .unwrap()
        .unwrap();
    assert_eq!(backup.content, b"original content");
    assert_eq!(backup.content_hash, "abc123");
    assert_eq!(backup.permissions, Some(0o644));
    assert!(!backup.was_symlink);
    assert!(!backup.oversized);
}

#[test]
fn file_backup_symlink() {
    let store = StateStore::open_in_memory().unwrap();
    let apply_id = store
        .record_apply("test", "hash", ApplyStatus::Success, None)
        .unwrap();

    let state = crate::FileState {
        content: Vec::new(),
        content_hash: String::new(),
        permissions: None,
        is_symlink: true,
        symlink_target: Some(PathBuf::from("/etc/original")),
        oversized: false,
    };

    store
        .store_file_backup(apply_id, "/home/user/link", &state)
        .unwrap();

    let backup = store
        .get_file_backup(apply_id, "/home/user/link")
        .unwrap()
        .unwrap();
    assert!(backup.was_symlink);
    assert_eq!(backup.symlink_target.unwrap(), "/etc/original");
}

#[test]
fn get_apply_backups_returns_all() {
    let store = StateStore::open_in_memory().unwrap();
    let apply_id = store
        .record_apply("test", "hash", ApplyStatus::Success, None)
        .unwrap();

    for i in 0..3 {
        let state = crate::FileState {
            content: format!("content {}", i).into_bytes(),
            content_hash: format!("hash{}", i),
            permissions: Some(0o644),
            is_symlink: false,
            symlink_target: None,
            oversized: false,
        };
        store
            .store_file_backup(apply_id, &format!("/file{}", i), &state)
            .unwrap();
    }

    let backups = store.get_apply_backups(apply_id).unwrap();
    assert_eq!(backups.len(), 3);
}

#[test]
fn latest_backup_for_path_returns_most_recent() {
    let store = StateStore::open_in_memory().unwrap();

    for i in 0..3 {
        let apply_id = store
            .record_apply("test", &format!("hash{}", i), ApplyStatus::Success, None)
            .unwrap();
        let state = crate::FileState {
            content: format!("content v{}", i).into_bytes(),
            content_hash: format!("hash{}", i),
            permissions: Some(0o644),
            is_symlink: false,
            symlink_target: None,
            oversized: false,
        };
        store
            .store_file_backup(apply_id, "/home/user/.bashrc", &state)
            .unwrap();
    }

    let backup = store
        .latest_backup_for_path("/home/user/.bashrc")
        .unwrap()
        .unwrap();
    assert_eq!(backup.content_hash, "hash2");
}

#[test]
fn journal_lifecycle() {
    let store = StateStore::open_in_memory().unwrap();
    let apply_id = store
        .record_apply("test", "hash", ApplyStatus::Success, None)
        .unwrap();

    let j1 = store
        .journal_begin(apply_id, 0, "files", "create", "/home/user/.bashrc", None)
        .unwrap();
    store.journal_complete(j1, Some("hash123"), None).unwrap();

    let j2 = store
        .journal_begin(apply_id, 1, "files", "update", "/home/user/.zshrc", None)
        .unwrap();
    store.journal_fail(j2, "permission denied").unwrap();

    // Script action with captured output
    let j3 = store
        .journal_begin(apply_id, 2, "scripts", "run", "setup.sh", None)
        .unwrap();
    store
        .journal_complete(j3, None, Some("installed deps\nall good"))
        .unwrap();

    // journal_entries returns all entries (ordered by action_index), including
    // failed ones, and preserves per-action status + captured script output.
    let all = store.journal_entries(apply_id).unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].resource_id, "/home/user/.bashrc");
    assert_eq!(all[0].status, "completed");
    assert!(all[0].script_output.is_none());
    assert_eq!(all[1].status, "failed");
    assert_eq!(all[2].resource_id, "setup.sh");
    assert_eq!(all[2].status, "completed");
    assert_eq!(
        all[2].script_output.as_deref(),
        Some("installed deps\nall good")
    );
}

#[test]
fn module_file_manifest_crud() {
    let store = StateStore::open_in_memory().unwrap();
    let apply_id = store
        .record_apply("test", "hash", ApplyStatus::Success, None)
        .unwrap();

    store
        .upsert_module_file(
            "nvim",
            "/home/user/.config/nvim/init.lua",
            "hash1",
            "Copy",
            apply_id,
        )
        .unwrap();
    store
        .upsert_module_file(
            "nvim",
            "/home/user/.config/nvim/lazy.lua",
            "hash2",
            "Copy",
            apply_id,
        )
        .unwrap();

    let files = store.module_deployed_files("nvim").unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].file_path, "/home/user/.config/nvim/init.lua");

    // Upsert updates existing
    store
        .upsert_module_file(
            "nvim",
            "/home/user/.config/nvim/init.lua",
            "newhash",
            "Symlink",
            apply_id,
        )
        .unwrap();
    let files = store.module_deployed_files("nvim").unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].content_hash, "newhash");
    assert_eq!(files[0].strategy, "Symlink");

    // Delete all
    store.delete_module_files("nvim").unwrap();
    let files = store.module_deployed_files("nvim").unwrap();
    assert!(files.is_empty());
}

#[test]
fn prune_old_backups_keeps_recent() {
    let store = StateStore::open_in_memory().unwrap();

    // Create 5 applies with backups
    for i in 0..5 {
        let apply_id = store
            .record_apply("test", &format!("hash{}", i), ApplyStatus::Success, None)
            .unwrap();
        let state = crate::FileState {
            content: format!("content {}", i).into_bytes(),
            content_hash: format!("hash{}", i),
            permissions: Some(0o644),
            is_symlink: false,
            symlink_target: None,
            oversized: false,
        };
        store.store_file_backup(apply_id, "/file", &state).unwrap();
    }

    // Prune keeping last 2
    let pruned = store.prune_old_backups(2).unwrap();
    assert_eq!(pruned, 3);

    // Only 2 backups remain
    let all: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM file_backups", [], |row| row.get(0))
        .unwrap();
    assert_eq!(all, 2);
}

#[test]
fn update_apply_status_works() {
    let store = StateStore::open_in_memory().unwrap();
    let apply_id = store
        .record_apply("test", "hash", ApplyStatus::Success, None)
        .unwrap();

    store
        .update_apply_status(apply_id, ApplyStatus::Partial, Some("{\"failed\":1}"))
        .unwrap();

    let record = store.last_apply().unwrap().unwrap();
    assert_eq!(record.status, ApplyStatus::Partial);
    assert_eq!(record.summary.unwrap(), "{\"failed\":1}");
}

#[test]
fn schema_version_advances_to_migration_count() {
    let store = StateStore::open_in_memory().unwrap();
    let version = store.schema_version();
    assert_eq!(
        version,
        super::MIGRATIONS.len(),
        "open must run every migration and advance schema_version to the count"
    );
}

#[test]
fn migration_adds_uninstall_cmd_column() {
    // Assert the column exists by writing and reading it back through the
    // package-resource helper, rather than pinning a fragile version number.
    let store = StateStore::open_in_memory().unwrap();
    store
        .upsert_package_resource("widgetmgr/widget", "local", None, Some("widgetmgr rm"))
        .unwrap();
    let known = std::collections::HashSet::new();
    let orphans = store.orphaned_package_resources(&known).unwrap();
    assert_eq!(orphans.len(), 1);
    assert_eq!(orphans[0].uninstall_cmd.as_deref(), Some("widgetmgr rm"));
}

// --- Compliance snapshot tests ---

fn make_test_snapshot() -> crate::compliance::ComplianceSnapshot {
    crate::compliance::ComplianceSnapshot {
        timestamp: crate::utc_now_iso8601(),
        machine: crate::compliance::MachineInfo {
            hostname: "test-host".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
        },
        profile: "default".into(),
        sources: vec!["local".into()],
        checks: vec![
            crate::compliance::ComplianceCheck {
                category: "file".into(),
                target: Some("/home/user/.zshrc".into()),
                status: crate::compliance::ComplianceStatus::Compliant,
                detail: Some("present".into()),
                ..Default::default()
            },
            crate::compliance::ComplianceCheck {
                category: "package".into(),
                name: Some("ripgrep".into()),
                status: crate::compliance::ComplianceStatus::Violation,
                detail: Some("not installed".into()),
                ..Default::default()
            },
            crate::compliance::ComplianceCheck {
                category: "system".into(),
                key: Some("shell".into()),
                status: crate::compliance::ComplianceStatus::Warning,
                detail: Some("no configurator".into()),
                ..Default::default()
            },
        ],
        summary: crate::compliance::ComplianceSummary {
            compliant: 1,
            warning: 1,
            violation: 1,
        },
    }
}

#[test]
fn compliance_snapshot_roundtrip() {
    let store = StateStore::open_in_memory().unwrap();
    let snapshot = make_test_snapshot();

    let (json, hash) = crate::compliance::snapshot_content_hash(&snapshot).unwrap();

    store.store_compliance_snapshot(&snapshot).unwrap();

    // The stored hash is the content digest of the stored JSON — the invariant
    // migration 14's rehash statement restores for rows written earlier.
    let latest = store.latest_compliance_hash().unwrap().unwrap();
    assert_eq!(latest, hash);
    assert_eq!(
        latest,
        crate::compliance::snapshot_json_content_hash(&json).unwrap()
    );

    // Retrieve full snapshot by history
    let history = store.compliance_history(None, 10).unwrap();
    assert_eq!(history.len(), 1);
    let row = &history[0];
    assert_eq!(row.compliant, 1);
    assert_eq!(row.warning, 1);
    assert_eq!(row.violation, 1);

    // Retrieve by ID
    let retrieved = store.get_compliance_snapshot(row.id).unwrap().unwrap();
    assert_eq!(retrieved.profile, "default");
    assert_eq!(retrieved.checks.len(), 3);
    assert_eq!(retrieved.summary.compliant, 1);
}

#[test]
fn compliance_latest_hash_empty() {
    let store = StateStore::open_in_memory().unwrap();
    assert!(store.latest_compliance_hash().unwrap().is_none());
}

#[test]
fn compliance_latest_hash_returns_most_recent() {
    let store = StateStore::open_in_memory().unwrap();

    let mut s1 = make_test_snapshot();
    s1.timestamp = "2026-01-01T00:00:00Z".into();
    store.store_compliance_snapshot(&s1).unwrap();

    let mut s2 = make_test_snapshot();
    s2.timestamp = "2026-01-02T00:00:00Z".into();
    store.store_compliance_snapshot(&s2).unwrap();

    let latest = store.latest_compliance_hash().unwrap().unwrap();
    assert_eq!(
        latest,
        crate::compliance::snapshot_content_hash(&s2).unwrap().1
    );
}

#[test]
fn compliance_prune_removes_old_snapshots() {
    let store = StateStore::open_in_memory().unwrap();

    let mut s1 = make_test_snapshot();
    s1.timestamp = "2026-01-01T00:00:00Z".into();
    store.store_compliance_snapshot(&s1).unwrap();

    let mut s2 = make_test_snapshot();
    s2.timestamp = "2026-01-15T00:00:00Z".into();
    store.store_compliance_snapshot(&s2).unwrap();

    let mut s3 = make_test_snapshot();
    s3.timestamp = "2026-02-01T00:00:00Z".into();
    store.store_compliance_snapshot(&s3).unwrap();

    // Prune everything before Feb
    let deleted = store
        .prune_compliance_snapshots("2026-02-01T00:00:00Z")
        .unwrap();
    assert_eq!(deleted, 2);

    let history = store.compliance_history(None, 10).unwrap();
    assert_eq!(history.len(), 1);
}

#[test]
fn compliance_history_with_since() {
    let store = StateStore::open_in_memory().unwrap();

    let mut s1 = make_test_snapshot();
    s1.timestamp = "2026-01-01T00:00:00Z".into();
    store.store_compliance_snapshot(&s1).unwrap();

    let mut s2 = make_test_snapshot();
    s2.timestamp = "2026-01-10T00:00:00Z".into();
    store.store_compliance_snapshot(&s2).unwrap();

    let mut s3 = make_test_snapshot();
    s3.timestamp = "2026-01-20T00:00:00Z".into();
    store.store_compliance_snapshot(&s3).unwrap();

    let history = store
        .compliance_history(Some("2026-01-05T00:00:00Z"), 10)
        .unwrap();
    assert_eq!(history.len(), 2);
}

#[test]
fn compliance_get_nonexistent() {
    let store = StateStore::open_in_memory().unwrap();
    assert!(store.get_compliance_snapshot(999).unwrap().is_none());
}

// --- Module state CRUD ---

#[test]
fn module_state_upsert_and_retrieve() {
    let store = StateStore::open_in_memory().unwrap();

    // Create apply records first (foreign key constraint)
    let apply1 = store
        .record_apply("default", "h1", ApplyStatus::Success, None)
        .unwrap();

    store
        .upsert_module_state(
            "nvim",
            Some(apply1),
            "pkg-hash-1",
            "file-hash-1",
            None,
            "installed",
        )
        .unwrap();
    store
        .upsert_module_state(
            "tmux",
            None,
            "pkg-hash-2",
            "file-hash-2",
            Some("https://github.com/example/tmux.git@abc123"),
            "installed",
        )
        .unwrap();

    let states = store.module_states().unwrap();
    assert_eq!(states.len(), 2);
    // Ordered by module_name
    assert_eq!(states[0].module_name, "nvim");
    assert_eq!(states[0].packages_hash, "pkg-hash-1");
    assert_eq!(states[0].files_hash, "file-hash-1");
    assert_eq!(states[0].status, "installed");
    assert_eq!(states[0].last_applied, Some(apply1));
    assert!(states[0].git_sources.is_none());

    assert_eq!(states[1].module_name, "tmux");
    assert!(states[1].last_applied.is_none());
    assert_eq!(
        states[1].git_sources.as_deref(),
        Some("https://github.com/example/tmux.git@abc123")
    );
}

#[test]
fn module_state_by_name_found_and_not_found() {
    let store = StateStore::open_in_memory().unwrap();

    let apply_id = store
        .record_apply("default", "h", ApplyStatus::Success, None)
        .unwrap();

    store
        .upsert_module_state("shell", Some(apply_id), "h1", "h2", None, "installed")
        .unwrap();

    let found = store.module_state_by_name("shell").unwrap();
    assert!(found.is_some());
    let rec = found.unwrap();
    assert_eq!(rec.module_name, "shell");
    assert_eq!(rec.last_applied, Some(apply_id));

    let not_found = store.module_state_by_name("nonexistent").unwrap();
    assert!(not_found.is_none());
}

#[test]
fn module_state_upsert_updates_on_conflict() {
    let store = StateStore::open_in_memory().unwrap();

    let apply1 = store
        .record_apply("default", "h1", ApplyStatus::Success, None)
        .unwrap();
    let apply2 = store
        .record_apply("default", "h2", ApplyStatus::Success, None)
        .unwrap();

    store
        .upsert_module_state(
            "nvim",
            Some(apply1),
            "old-pkg",
            "old-file",
            None,
            "installed",
        )
        .unwrap();
    store
        .upsert_module_state("nvim", Some(apply2), "new-pkg", "new-file", None, "updated")
        .unwrap();

    let states = store.module_states().unwrap();
    assert_eq!(
        states.len(),
        1,
        "upsert should update, not insert duplicate"
    );
    assert_eq!(states[0].packages_hash, "new-pkg");
    assert_eq!(states[0].files_hash, "new-file");
    assert_eq!(states[0].status, "updated");
    assert_eq!(states[0].last_applied, Some(apply2));
}

#[test]
fn module_state_remove() {
    let store = StateStore::open_in_memory().unwrap();

    store
        .upsert_module_state("nvim", None, "h1", "h2", None, "installed")
        .unwrap();
    store
        .upsert_module_state("tmux", None, "h3", "h4", None, "installed")
        .unwrap();

    assert_eq!(store.module_states().unwrap().len(), 2);

    store.remove_module_state("nvim").unwrap();
    let states = store.module_states().unwrap();
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].module_name, "tmux");

    // Removing nonexistent module should not error
    store.remove_module_state("nonexistent").unwrap();
    assert_eq!(store.module_states().unwrap().len(), 1);
}

// --- record_source_apply ---

#[test]
fn record_source_apply_links_to_source() {
    let store = StateStore::open_in_memory().unwrap();

    // Create a source first
    store
        .upsert_config_source(
            "acme",
            "https://github.com/acme/config.git",
            "main",
            None,
            None,
            None,
        )
        .unwrap();

    // Record an apply
    let apply_id = store
        .record_apply("default", "plan-hash-1", ApplyStatus::Success, None)
        .unwrap();
    store
        .record_source_apply("acme", apply_id, "abc123def")
        .unwrap();

    // Verify the source exists and was linked
    let source = store.config_source_by_name("acme").unwrap();
    assert!(source.is_some());
}

#[test]
fn record_source_apply_nonexistent_source_is_noop() {
    let store = StateStore::open_in_memory().unwrap();

    let apply_id = store
        .record_apply("default", "plan-hash-1", ApplyStatus::Success, None)
        .unwrap();

    // Recording for a nonexistent source should be a no-op (not an error)
    store
        .record_source_apply("nonexistent", apply_id, "abc123")
        .unwrap();

    // Verify no rows were inserted into source_applies
    let count: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM source_applies", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        count, 0,
        "no source_applies row should exist for nonexistent source"
    );

    // Verify the source still doesn't exist
    let source = store.config_source_by_name("nonexistent").unwrap();
    assert!(source.is_none(), "nonexistent source should not be created");
}

#[test]
fn remove_config_source_after_apply_cascades_source_applies() {
    // An apply records a source_applies row referencing the source. Removing the
    // source must succeed and cascade-delete that row. Before source_id gained
    // ON DELETE CASCADE the bare DELETE on config_sources failed the foreign-key
    // check (foreign_keys=ON), so `source remove`/`source replace` died after any
    // apply — and the cfgd.yaml mutation had already landed, leaving config and
    // state inconsistent.
    let store = StateStore::open_in_memory().unwrap();
    store
        .upsert_config_source(
            "acme",
            "https://example.invalid/acme",
            "master",
            Some("abc123"),
            Some("1.0.0"),
            None,
        )
        .unwrap();
    let apply_id = store
        .record_apply("default", "plan-hash-1", ApplyStatus::Success, None)
        .unwrap();
    store
        .record_source_apply("acme", apply_id, "abc123")
        .unwrap();

    let before: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM source_applies", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        before, 1,
        "the apply should have linked a source_applies row"
    );

    // Previously returned a FOREIGN KEY constraint error.
    store.remove_config_source("acme").unwrap();

    assert!(
        store.config_source_by_name("acme").unwrap().is_none(),
        "the source row must be gone"
    );
    let after: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM source_applies", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        after, 0,
        "source_applies rows must cascade-delete when their source is removed"
    );
}

#[test]
fn migration_6_rebuilds_source_applies_preserving_rows_and_enabling_cascade() {
    // The upgrade path: an existing pre-fix DB (schema_version 5) has a
    // source_applies row whose FK lacks ON DELETE CASCADE. Reopening runs
    // migration 6, which must (a) preserve the existing row through the table
    // rebuild and (b) leave the FK with ON DELETE CASCADE so removal works.
    //
    // The fixture REPLAYS the first five migrations rather than declaring the
    // schema by hand, because reopening replays the entire tail and a later
    // migration touching a table the hand-written DDL forgot would fail on a
    // shape no real version-5 database has. Replaying is also what produces the
    // cascade-less `source_applies` this test is about — migration 6 is the one
    // that adds ON DELETE CASCADE.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        for migration in &MIGRATIONS[..5] {
            conn.execute_batch(migration).unwrap();
        }
        conn.execute_batch(
            "UPDATE schema_version SET version = 5;
             INSERT INTO config_sources (id, name, origin_url) VALUES (1, 'acme', 'u');
             INSERT INTO applies (id, timestamp, profile, plan_hash, status)
                VALUES (1, 't', 'default', 'h', 'success');
             INSERT INTO source_applies (id, source_id, apply_id, source_commit)
                VALUES (7, 1, 1, 'abc123');",
        )
        .unwrap();
    }

    // Reopen — runs migration 6 (the rebuild).
    let store = StateStore::open(&path).unwrap();

    // (a) the existing row survived, identity intact.
    let row: (i64, i64, String) = store
        .conn
        .query_row(
            "SELECT id, source_id, source_commit FROM source_applies",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        row,
        (7, 1, "abc123".to_string()),
        "row must survive rebuild"
    );

    // (b) the FK now cascades: removing the source drops its source_applies row.
    store.remove_config_source("acme").unwrap();
    let after: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM source_applies", [], |r| r.get(0))
        .unwrap();
    assert_eq!(after, 0, "migration 6 must enable ON DELETE CASCADE");
}

#[test]
fn migration_9_drops_stale_managed_resource_ids_and_apply_recreates_them() {
    use crate::providers::ProviderRegistry;
    use crate::reconciler::{
        Action, ModuleAction, ModuleActionKind, Phase, PhaseName, Plan, ReconcileContext,
        Reconciler,
    };

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");

    // Seed rows in the shapes the old id derivations produced, then wind
    // schema_version back one step so reopening replays the sweep migration.
    // Building the pre-migration schema by hand instead would duplicate every
    // earlier migration's DDL and rot the moment one of them changes.
    {
        let store = StateStore::open(&path).unwrap();
        // Every module collapsed onto the bare verb, so the module name was lost.
        store
            .upsert_managed_resource("module", "script", "local", None, None)
            .unwrap();
        // Truncated at the colon inside the script body.
        store
            .upsert_managed_resource("Running script", " curl https", "local", None, None)
            .unwrap();
        // Truncated at the colon inside the configurator value.
        store
            .upsert_managed_resource("system", "path.value (a", "local", None, None)
            .unwrap();
        // Native-separator secret key, as a Windows host would have written it.
        store
            .upsert_managed_resource("secret", r"C:\Users\me\.env", "local", None, None)
            .unwrap();
        // Every manager's bootstrap/skip collapsed onto the bare verb, losing
        // the manager name. These go through upsert_managed_resource, so they
        // carry no uninstall_cmd.
        store
            .upsert_managed_resource("package", "skip", "local", None, None)
            .unwrap();
        store
            .upsert_managed_resource("package", "bootstrap", "local", None, None)
            .unwrap();
        // A real package row must NOT be swept — it is the one shape carrying an
        // uninstall_cmd that cannot be re-derived once its manager leaves config.
        store
            .upsert_package_resource("widgetmgr/widget", "local", None, Some("widgetmgr rm"))
            .unwrap();
        // Hardcoded, not `MIGRATIONS.len() - 1`: this test means "replay the
        // id-shape sweep", so appending a later migration must not silently
        // re-point it at the new tail.
        store
            .conn
            .execute("UPDATE schema_version SET version = 8", [])
            .unwrap();
    }

    let state = StateStore::open(&path).unwrap();

    let swept = state.managed_resources().unwrap();
    assert!(
        swept
            .iter()
            .all(|r| !["module", "Running script", "system", "secret"]
                .contains(&r.resource_type.as_str())),
        "migration 9 must remove every row whose id shape changed: {swept:?}"
    );
    assert!(
        !swept
            .iter()
            .any(|r| r.resource_type == "package"
                && ["bootstrap", "skip"].contains(&&*r.resource_id)),
        "the collapsed package bootstrap/skip ids must be swept: {swept:?}"
    );
    let known = std::collections::HashSet::new();
    let orphans = state.orphaned_package_resources(&known).unwrap();
    assert_eq!(
        orphans.len(),
        1,
        "the sweep must spare real package rows: {orphans:?}"
    );
    assert_eq!(orphans[0].package, "widget");
    assert_eq!(orphans[0].uninstall_cmd.as_deref(), Some("widgetmgr rm"));

    // A fresh apply re-derives the rows under the corrected id — and two
    // modules no longer contend for the same UNIQUE(resource_type, resource_id).
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = crate::test_helpers::make_empty_resolved();
    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Modules,
            &crate::reconciler::Owner::profile("test"),
            ["nvim", "zsh"]
                .into_iter()
                .map(|name| {
                    Action::Module(ModuleAction {
                        module_name: name.to_string(),
                        kind: ModuleActionKind::Skip {
                            reason: "platform not matched".to_string(),
                        },
                        origin: None,
                    })
                })
                .collect(),
        )],
        warnings: vec![],
    };
    let printer = crate::test_helpers::test_printer();
    reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&crate::reconciler::PhaseFilter::Phase(PhaseName::Modules)),
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    let module_ids: Vec<String> = state
        .managed_resources()
        .unwrap()
        .into_iter()
        .filter(|r| r.resource_type == "module")
        .map(|r| r.resource_id)
        .collect();
    assert_eq!(
        module_ids,
        vec!["nvim:skip".to_string(), "zsh:skip".to_string()],
        "each module must re-appear under its own name-qualified id"
    );
}

#[test]
fn migration_10_folds_windows_file_path_keys_and_spares_unix_backslash_names() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let backup = |content: &[u8]| crate::FileState {
        content: content.to_vec(),
        content_hash: crate::sha256_hex(content),
        permissions: None,
        is_symlink: false,
        symlink_target: None,
        oversized: false,
    };

    {
        let store = StateStore::open(&path).unwrap();
        let apply_id = store
            .record_apply("default", "h", ApplyStatus::Success, None)
            .unwrap();
        // Keys as a pre-fold Windows host wrote them.
        store
            .store_file_backup(apply_id, r"C:\Users\me\.gitconfig", &backup(b"win"))
            .unwrap();
        store
            .store_file_backup(apply_id, r"\\srv\share\hosts", &backup(b"unc"))
            .unwrap();
        // A legal unix filename that merely contains a backslash: folding it
        // would re-point the row at a different file, so it must survive exact.
        store
            .store_file_backup(apply_id, r"/home/me/od\d.conf", &backup(b"nix"))
            .unwrap();
        // A Windows key authored with mixed separators is still Windows-rooted,
        // so its trailing backslashes have to fold with the rest.
        store
            .store_file_backup(apply_id, r"C:/Users/me\nvim\init.vim", &backup(b"mix"))
            .unwrap();
        store
            .upsert_module_file("nvim", r"C:\Users\me\init.lua", "h1", "Copy", apply_id)
            .unwrap();
        store
            .upsert_module_file("nvim", "C:/Users/me/init.lua", "h2", "Copy", apply_id)
            .unwrap();
        store
            .upsert_module_file("zsh", r"/home/me/od\d.zshrc", "h3", "Copy", apply_id)
            .unwrap();
        store
            .conn
            .execute("UPDATE schema_version SET version = 9", [])
            .unwrap();
    }

    let state = StateStore::open(&path).unwrap();

    assert!(
        state
            .latest_backup_for_path("C:/Users/me/.gitconfig")
            .unwrap()
            .is_some(),
        "a native-separator backup key must be reachable under its folded form"
    );
    assert!(
        state
            .latest_backup_for_path(r"C:\Users\me\.gitconfig")
            .unwrap()
            .is_none(),
        "the native-separator key must not survive alongside the folded one"
    );
    assert!(
        state
            .latest_backup_for_path("//srv/share/hosts")
            .unwrap()
            .is_some(),
        "a UNC key must fold too"
    );
    assert!(
        state
            .latest_backup_for_path(r"/home/me/od\d.conf")
            .unwrap()
            .is_some(),
        "a unix filename containing a backslash must be left exact"
    );
    assert!(
        state
            .latest_backup_for_path("C:/Users/me/nvim/init.vim")
            .unwrap()
            .is_some(),
        "a Windows key authored with mixed separators must fold whole"
    );

    let nvim = state.module_deployed_files("nvim").unwrap();
    assert_eq!(
        nvim.len(),
        1,
        "folding must collapse the two shapes onto one manifest row: {nvim:?}"
    );
    assert_eq!(nvim[0].file_path, "C:/Users/me/init.lua");
    assert_eq!(
        nvim[0].content_hash, "h1",
        "OR REPLACE keeps the row being folded and drops the twin it collides with"
    );

    let zsh = state.module_deployed_files("zsh").unwrap();
    assert_eq!(zsh.len(), 1);
    assert_eq!(
        zsh[0].file_path, r"/home/me/od\d.zshrc",
        "a unix manifest key containing a backslash must be left exact"
    );
}

// --- file_backups_after_apply ---

#[test]
fn file_backups_after_apply_returns_earliest_per_path() {
    let store = StateStore::open_in_memory().unwrap();

    let apply1 = store
        .record_apply("default", "hash1", ApplyStatus::Success, None)
        .unwrap();
    let apply2 = store
        .record_apply("default", "hash2", ApplyStatus::Success, None)
        .unwrap();
    let apply3 = store
        .record_apply("default", "hash3", ApplyStatus::Success, None)
        .unwrap();

    // Backup same file at apply2 and apply3
    let state_v1 = crate::FileState {
        content: b"version1".to_vec(),
        content_hash: "hash-v1".into(),
        permissions: None,
        is_symlink: false,
        symlink_target: None,
        oversized: false,
    };
    let state_v2 = crate::FileState {
        content: b"version2".to_vec(),
        content_hash: "hash-v2".into(),
        permissions: None,
        is_symlink: false,
        symlink_target: None,
        oversized: false,
    };

    store
        .store_file_backup(apply2, "/etc/config", &state_v1)
        .unwrap();
    store
        .store_file_backup(apply3, "/etc/config", &state_v2)
        .unwrap();

    // Backups after apply1 should return the EARLIEST backup per path (apply2's version)
    let backups = store.file_backups_after_apply(apply1).unwrap();
    assert_eq!(backups.len(), 1);
    assert_eq!(backups[0].file_path, "/etc/config");
    assert_eq!(backups[0].apply_id, apply2);
    assert_eq!(backups[0].content_hash, "hash-v1");

    // Backups after apply2 should return apply3's version
    let backups_after_2 = store.file_backups_after_apply(apply2).unwrap();
    assert_eq!(backups_after_2.len(), 1);
    assert_eq!(backups_after_2[0].apply_id, apply3);
    assert_eq!(backups_after_2[0].content_hash, "hash-v2");

    // Backups after apply3 should be empty
    let backups_after_3 = store.file_backups_after_apply(apply3).unwrap();
    assert!(backups_after_3.is_empty());
}

#[test]
fn store_absent_backup_round_trips_with_existed_false() {
    let store = StateStore::open_in_memory().unwrap();
    let apply_id = store
        .record_apply("test", "hash", ApplyStatus::Success, None)
        .unwrap();

    store
        .store_absent_backup(apply_id, "/home/user/new-file")
        .unwrap();

    let backup = store
        .get_file_backup(apply_id, "/home/user/new-file")
        .unwrap()
        .unwrap();
    assert!(
        !backup.existed,
        "absent marker must record existed=false so rollback removes the file"
    );
    assert!(backup.content.is_empty());
    assert_eq!(backup.content_hash, crate::sha256_hex(b""));
    assert_eq!(backup.permissions, None);
    assert!(!backup.was_symlink);
    assert!(!backup.oversized);
}

#[test]
fn store_file_backup_records_existed_true() {
    let store = StateStore::open_in_memory().unwrap();
    let apply_id = store
        .record_apply("test", "hash", ApplyStatus::Success, None)
        .unwrap();

    let state = crate::FileState {
        content: b"present".to_vec(),
        content_hash: "h".into(),
        permissions: Some(0o644),
        is_symlink: false,
        symlink_target: None,
        oversized: false,
    };
    store
        .store_file_backup(apply_id, "/home/user/present", &state)
        .unwrap();

    let backup = store
        .get_file_backup(apply_id, "/home/user/present")
        .unwrap()
        .unwrap();
    assert!(backup.existed, "real backups must record existed=true");
}

#[test]
fn get_apply_backups_surfaces_existed_field() {
    let store = StateStore::open_in_memory().unwrap();
    let apply_id = store
        .record_apply("test", "hash", ApplyStatus::Success, None)
        .unwrap();

    let present = crate::FileState {
        content: b"x".to_vec(),
        content_hash: "h".into(),
        permissions: None,
        is_symlink: false,
        symlink_target: None,
        oversized: false,
    };
    store
        .store_file_backup(apply_id, "/present", &present)
        .unwrap();
    store.store_absent_backup(apply_id, "/absent").unwrap();

    let backups = store.get_apply_backups(apply_id).unwrap();
    let present_rec = backups.iter().find(|b| b.file_path == "/present").unwrap();
    let absent_rec = backups.iter().find(|b| b.file_path == "/absent").unwrap();
    assert!(present_rec.existed);
    assert!(!absent_rec.existed);
}

#[test]
fn file_backups_after_apply_surfaces_existed_field() {
    let store = StateStore::open_in_memory().unwrap();
    let apply1 = store
        .record_apply("test", "h1", ApplyStatus::Success, None)
        .unwrap();
    let apply2 = store
        .record_apply("test", "h2", ApplyStatus::Success, None)
        .unwrap();

    store.store_absent_backup(apply2, "/created-later").unwrap();

    let backups = store.file_backups_after_apply(apply1).unwrap();
    assert_eq!(backups.len(), 1);
    assert_eq!(backups[0].file_path, "/created-later");
    assert!(
        !backups[0].existed,
        "absent marker must surface existed=false through the rollback fallback query"
    );
}

#[test]
fn migration_8_defaults_existed_to_one_for_legacy_rows() {
    // Legacy-shaped INSERTs that omit the `existed` column must default to 1
    // so every pre-migration backup keeps today's content-restore behavior.
    let store = StateStore::open_in_memory().unwrap();
    let apply_id = store
        .record_apply("test", "hash", ApplyStatus::Success, None)
        .unwrap();

    store
        .conn
        .execute(
            "INSERT INTO file_backups (apply_id, file_path, content_hash, content, was_symlink, oversized, backed_up_at)
             VALUES (?1, ?2, ?3, ?4, 0, 0, ?5)",
            rusqlite::params![apply_id, "/legacy", "h", b"data".to_vec(), crate::utc_now_iso8601()],
        )
        .unwrap();

    let backup = store.get_file_backup(apply_id, "/legacy").unwrap().unwrap();
    assert!(
        backup.existed,
        "migration 8 default must keep legacy rows at existed=1"
    );
}

// --- journal_entries_after_apply ---

#[test]
fn journal_entries_after_apply_returns_completed_desc() {
    let store = StateStore::open_in_memory().unwrap();

    let apply1 = store
        .record_apply("default", "hash1", ApplyStatus::Success, None)
        .unwrap();
    let apply2 = store
        .record_apply("default", "hash2", ApplyStatus::Success, None)
        .unwrap();

    // Journal entries for apply2
    let j1 = store
        .journal_begin(apply2, 0, "Packages", "install", "brew:curl", None)
        .unwrap();
    store.journal_complete(j1, None, None).unwrap();
    let j2 = store
        .journal_begin(apply2, 1, "Packages", "install", "brew:wget", None)
        .unwrap();
    store.journal_complete(j2, None, None).unwrap();
    // A failed entry should NOT be returned
    let j3 = store
        .journal_begin(apply2, 2, "Packages", "install", "brew:vim", None)
        .unwrap();
    store.journal_fail(j3, "package not found").unwrap();

    let entries = store.journal_entries_after_apply(apply1).unwrap();
    assert_eq!(
        entries.len(),
        2,
        "should return only completed entries, not failed"
    );
    // Results are ordered by apply_id DESC, action_index DESC
    assert_eq!(entries[0].resource_id, "brew:wget");
    assert_eq!(entries[1].resource_id, "brew:curl");
    assert_eq!(entries[0].status, "completed");
    assert_eq!(entries[1].status, "completed");
}

// --- concurrent in-memory stores ---

#[test]
fn concurrent_in_memory_stores_are_independent() {
    let store_a = StateStore::open_in_memory().unwrap();
    let store_b = StateStore::open_in_memory().unwrap();

    store_a
        .record_apply("default", "hash-a", ApplyStatus::Success, None)
        .unwrap();

    // store_b should be empty — separate database
    assert!(store_b.last_apply().unwrap().is_none());
    assert_eq!(store_a.history(10).unwrap().len(), 1);
    assert_eq!(store_b.history(10).unwrap().len(), 0);
}

// --- schema migration ---

#[test]
fn schema_version_after_open() {
    let store = StateStore::open_in_memory().unwrap();
    let version = store.schema_version();
    assert!(
        version >= 4,
        "schema version should be at least 4 after migrations: got {version}"
    );
}

#[test]
fn migration_13_reaches_a_database_already_past_the_backup_runs_insertion_point() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");

    // Simulate a database last migrated by a build that predates
    // `backup_runs`: every earlier table exists and schema_version already
    // stands at 12. The migration runner is positional, so `backup_runs` must
    // sit at the array tail — inserted mid-array it would never run for this
    // database. Dropping the table instead of rebuilding the old schema by
    // hand keeps the fixture from rotting as earlier migrations change.
    {
        let store = StateStore::open(&path).unwrap();
        store.conn.execute("DROP TABLE backup_runs", []).unwrap();
        store
            .conn
            .execute("UPDATE schema_version SET version = 12", [])
            .unwrap();
    }

    let state = StateStore::open(&path).unwrap();
    assert!(
        state.backup_runs("any").unwrap().is_empty(),
        "backup_runs must exist and be readable after replaying the tail migration"
    );
    assert_eq!(state.schema_version() as usize, MIGRATIONS.len());
}

fn doubled_prefix_snapshot(key: &str) -> crate::compliance::ComplianceSnapshot {
    use crate::compliance::{
        ComplianceCheck, ComplianceSnapshot, ComplianceStatus, ComplianceSummary, MachineInfo,
    };
    ComplianceSnapshot {
        timestamp: crate::utc_now_iso8601(),
        machine: MachineInfo {
            hostname: "host".to_string(),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
        },
        profile: "default".to_string(),
        sources: vec![],
        checks: vec![ComplianceCheck {
            category: "system".to_string(),
            key: Some(key.to_string()),
            status: ComplianceStatus::Violation,
            ..Default::default()
        }],
        summary: ComplianceSummary {
            compliant: 0,
            warning: 0,
            violation: 1,
        },
    }
}

#[test]
fn migration_14_undoubles_the_configurator_name_in_persisted_system_ids() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");

    {
        let store = StateStore::open(&path).unwrap();
        let apply_id = store
            .record_apply("default", "h", ApplyStatus::Success, None)
            .unwrap();

        // Ids as the self-prefixing configurators wrote them.
        store
            .upsert_managed_resource(
                "system",
                "sshKeys.sshKeys.default.exists",
                "local",
                None,
                Some(apply_id),
            )
            .unwrap();
        store
            .record_drift(
                "system",
                "seccomp.seccomp.default-audit",
                Some("present"),
                Some("missing"),
                "local",
            )
            .unwrap();
        store
            .journal_begin(
                apply_id,
                0,
                "System",
                "system",
                "kubelet.kubelet.maxPods",
                None,
            )
            .unwrap();
        store
            .store_compliance_snapshot(&doubled_prefix_snapshot(
                "apparmor.apparmor.test-profile.file",
            ))
            .unwrap();
        // A row whose stored hash does not describe its stored JSON — the shape
        // every snapshot the daemon wrote before the hash derivation was unified
        // carries, and one the public API can no longer produce.
        let (stale_json, _) = crate::compliance::snapshot_content_hash(&doubled_prefix_snapshot(
            "cert.kubelet-client.cert.mode",
        ))
        .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO compliance_snapshots (timestamp, content_hash, snapshot_json,
                    summary_compliant, summary_warning, summary_violation)
                 VALUES ('2026-01-01T00:00:00Z', 'stale-hash', ?1, 0, 0, 1)",
                rusqlite::params![stale_json],
            )
            .unwrap();
        // A snapshot that will not parse: the rehash must leave its hash alone
        // rather than fail, which inside the runner's EXCLUSIVE transaction
        // would roll the whole migration back and leave the store unopenable.
        store
            .conn
            .execute(
                "INSERT INTO compliance_snapshots (timestamp, content_hash, snapshot_json,
                    summary_compliant, summary_warning, summary_violation)
                 VALUES ('2026-01-01T00:00:01Z', 'corrupt-row-hash', '{not json', 0, 0, 0)",
                [],
            )
            .unwrap();

        // Controls. A configurator whose key prefix merely RESEMBLES its name
        // (`certificates` → `cert.…`) is not doubled; a longer name sharing the
        // doubled prefix's opening (`containerd.containerdx`) has no second
        // segment boundary; a lowercased twin proves the match is case-SENSITIVE
        // (SQLite's LIKE is not, GLOB is); and a non-`system` row proves the
        // rewrite is scoped by resource type rather than by id shape alone.
        store
            .upsert_managed_resource(
                "system",
                "certificates.cert.kubelet-client.cert",
                "local",
                None,
                Some(apply_id),
            )
            .unwrap();
        store
            .upsert_managed_resource(
                "system",
                "containerd.containerdx",
                "local",
                None,
                Some(apply_id),
            )
            .unwrap();
        store
            .upsert_managed_resource(
                "system",
                "sshkeys.sshkeys.default.exists",
                "local",
                None,
                Some(apply_id),
            )
            .unwrap();
        store
            .record_drift(
                "file",
                "seccomp.seccomp.default-audit",
                None,
                Some("modified"),
                "local",
            )
            .unwrap();

        // Hardcoded, not `MIGRATIONS.len() - 1`: this test means "replay the
        // id-undoubling rewrite", so appending a later migration must not
        // silently re-point it at the new tail.
        store
            .conn
            .execute("UPDATE schema_version SET version = 13", [])
            .unwrap();
    }

    let state = StateStore::open(&path).unwrap();

    let ids: Vec<String> = state
        .managed_resources()
        .unwrap()
        .into_iter()
        .filter(|r| r.resource_type == "system")
        .map(|r| r.resource_id)
        .collect();
    assert!(
        ids.contains(&"sshKeys.default.exists".to_string()),
        "the doubled managed_resources id must be rewritten: {ids:?}"
    );
    assert!(
        ids.contains(&"certificates.cert.kubelet-client.cert".to_string())
            && ids.contains(&"containerd.containerdx".to_string())
            && ids.contains(&"sshkeys.sshkeys.default.exists".to_string()),
        "no control id may be rewritten: {ids:?}"
    );

    let drift: Vec<(String, String)> = state
        .unresolved_drift()
        .unwrap()
        .into_iter()
        .map(|d| (d.resource_type, d.resource_id))
        .collect();
    assert!(
        drift.contains(&("system".to_string(), "seccomp.default-audit".to_string())),
        "the doubled drift id must be rewritten: {drift:?}"
    );
    assert!(
        drift.contains(&(
            "file".to_string(),
            "seccomp.seccomp.default-audit".to_string()
        )),
        "a non-system drift row must be left alone: {drift:?}"
    );

    let journal_ids: Vec<String> = state
        .journal_entries(1)
        .unwrap()
        .into_iter()
        .map(|e| e.resource_id)
        .collect();
    assert_eq!(journal_ids, vec!["kubelet.maxPods".to_string()]);

    let rewritten = state
        .compliance_history(None, 10)
        .unwrap()
        .into_iter()
        // The deliberately-corrupt row does not deserialize; every other row must.
        .filter_map(|row| state.get_compliance_snapshot(row.id).ok().flatten())
        .map(|snapshot| snapshot.checks[0].key.clone().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(rewritten.len(), 2);
    assert!(
        rewritten.contains(&"apparmor.test-profile.file".to_string()),
        "the doubled compliance check key must be rewritten: {rewritten:?}"
    );
    assert!(
        rewritten.contains(&"cert.kubelet-client.cert.mode".to_string()),
        "an undoubled check key must survive verbatim: {rewritten:?}"
    );

    // Every row's hash must describe the row: the rewritten snapshot because its
    // JSON moved, and the stale-hash row because nothing else ever repairs it.
    let hashes: Vec<(String, String)> = state
        .conn
        .prepare("SELECT content_hash, snapshot_json FROM compliance_snapshots")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(hashes.len(), 3);
    for (hash, json) in &hashes {
        match crate::compliance::snapshot_json_content_hash(json) {
            Ok(derived) => assert_eq!(
                hash, &derived,
                "content_hash must be re-derived from the stored JSON"
            ),
            Err(_) => assert_eq!(
                hash, "corrupt-row-hash",
                "an unparseable snapshot must keep the hash it had"
            ),
        }
    }

    assert_eq!(state.schema_version() as usize, MIGRATIONS.len());
}

#[test]
fn migration_14_is_a_no_op_on_a_fresh_store() {
    let store = StateStore::open_in_memory().unwrap();
    assert_eq!(store.schema_version(), MIGRATIONS.len());
    assert!(store.managed_resources().unwrap().is_empty());
    assert!(store.unresolved_drift().unwrap().is_empty());
    assert!(store.compliance_history(None, 10).unwrap().is_empty());
}

// --- get_apply by id ---

#[test]
fn get_apply_existing_and_nonexistent() {
    let store = StateStore::open_in_memory().unwrap();

    let apply_id = store
        .record_apply(
            "default",
            "plan-hash",
            ApplyStatus::Success,
            Some("{\"summary\": true}"),
        )
        .unwrap();

    let found = store.get_apply(apply_id).unwrap();
    assert!(found.is_some());
    let rec = found.unwrap();
    assert_eq!(rec.id, apply_id);
    assert_eq!(rec.plan_hash, "plan-hash");
    assert_eq!(rec.status, ApplyStatus::Success);
    assert_eq!(rec.summary.as_deref(), Some("{\"summary\": true}"));

    let not_found = store.get_apply(99999).unwrap();
    assert!(not_found.is_none());
}

// --- update_apply_status ---

#[test]
fn update_apply_status_changes_status() {
    let store = StateStore::open_in_memory().unwrap();

    let apply_id = store
        .record_apply("default", "hash", ApplyStatus::InProgress, None)
        .unwrap();

    store
        .update_apply_status(apply_id, ApplyStatus::Success, Some("{\"total\": 5}"))
        .unwrap();

    let rec = store.get_apply(apply_id).unwrap().unwrap();
    assert_eq!(rec.status, ApplyStatus::Success);
    assert_eq!(rec.summary.as_deref(), Some("{\"total\": 5}"));
}

// --- migrate_state_db ---

#[test]
fn migrate_state_db_moves_db_and_reopens() {
    let legacy = tempfile::tempdir().unwrap();
    let new = tempfile::tempdir().unwrap();
    let new_dir = new.path().join("state");

    // Seed a schema-bearing DB at the legacy location.
    {
        let store = StateStore::open_in_dir(legacy.path()).unwrap();
        store
            .record_apply("default", "h1", ApplyStatus::Success, Some("seed"))
            .unwrap();
    }

    let migrated = migrate_state_db(legacy.path(), &new_dir).unwrap();
    assert!(migrated, "a DB present at legacy must report migrated=true");
    assert!(
        new_dir.join(STATE_DB_FILENAME).exists(),
        "DB must exist at new dir"
    );
    assert!(
        !legacy.path().join(STATE_DB_FILENAME).exists(),
        "legacy DB must be gone"
    );

    // Reopens cleanly at the new location.
    let store = StateStore::open_in_dir(&new_dir).unwrap();
    assert!(store.last_apply().unwrap().is_some());
}

#[test]
fn migrate_state_db_preserves_committed_rows() {
    let legacy = tempfile::tempdir().unwrap();
    let new = tempfile::tempdir().unwrap();
    let new_dir = new.path().join("state");

    // Write through the real store (WAL mode) and close it so the row is
    // committed but possibly still in the WAL.
    {
        let store = StateStore::open_in_dir(legacy.path()).unwrap();
        store
            .record_apply("prod", "deadbeef", ApplyStatus::Success, Some("survives"))
            .unwrap();
    }

    assert!(migrate_state_db(legacy.path(), &new_dir).unwrap());

    let store = StateStore::open_in_dir(&new_dir).unwrap();
    let rec = store
        .last_apply()
        .unwrap()
        .expect("the committed row must survive the move");
    assert_eq!(rec.profile, "prod");
    assert_eq!(rec.plan_hash, "deadbeef");
    assert_eq!(rec.summary.as_deref(), Some("survives"));
}

#[test]
fn migrate_state_db_never_clobbers_existing_new_db() {
    let legacy = tempfile::tempdir().unwrap();
    let new = tempfile::tempdir().unwrap();
    let new_dir = new.path().join("state");
    std::fs::create_dir_all(&new_dir).unwrap();

    // Distinct content already at the destination.
    let new_db = new_dir.join(STATE_DB_FILENAME);
    std::fs::write(&new_db, b"DO-NOT-OVERWRITE").unwrap();

    // A real DB at legacy.
    {
        let store = StateStore::open_in_dir(legacy.path()).unwrap();
        store
            .record_apply("x", "y", ApplyStatus::Success, None)
            .unwrap();
    }

    let migrated = migrate_state_db(legacy.path(), &new_dir).unwrap();
    assert!(!migrated, "an existing new DB must short-circuit to false");
    assert_eq!(std::fs::read(&new_db).unwrap(), b"DO-NOT-OVERWRITE");
    assert!(
        legacy.path().join(STATE_DB_FILENAME).exists(),
        "legacy DB stays put"
    );
}

#[test]
fn migrate_state_db_no_legacy_db_is_noop() {
    let legacy = tempfile::tempdir().unwrap();
    let new = tempfile::tempdir().unwrap();
    let migrated = migrate_state_db(legacy.path(), &new.path().join("state")).unwrap();
    assert!(!migrated, "no legacy DB means nothing to migrate");
}

#[test]
fn migrate_state_db_preserves_sidecars_when_checkpoint_fails() {
    let legacy = tempfile::tempdir().unwrap();
    let new = tempfile::tempdir().unwrap();
    let new_dir = new.path().join("state");

    // A legacy "db" whose bytes are NOT a valid SQLite file: Connection::open
    // succeeds (open is lazy) but the wal_checkpoint PRAGMA fails, deterministically
    // forcing the checkpoint-failure branch on Linux without any lock-timing races.
    let legacy_db = legacy.path().join(STATE_DB_FILENAME);
    std::fs::write(&legacy_db, b"this is not a sqlite database").unwrap();
    std::fs::write(
        legacy.path().join(format!("{STATE_DB_FILENAME}-wal")),
        b"wal-bytes",
    )
    .unwrap();
    std::fs::write(
        legacy.path().join(format!("{STATE_DB_FILENAME}-shm")),
        b"shm-bytes",
    )
    .unwrap();

    let migrated = migrate_state_db(legacy.path(), &new_dir).unwrap();
    assert!(migrated, "a present legacy DB still reports migrated=true");
    assert!(
        new_dir.join(STATE_DB_FILENAME).exists(),
        "main DB must land at new dir"
    );
    // The checkpoint failed, so committed-but-unfolded sidecars must be carried
    // across (with their bytes intact) rather than dropped.
    assert_eq!(
        std::fs::read(new_dir.join(format!("{STATE_DB_FILENAME}-wal"))).unwrap(),
        b"wal-bytes",
        "WAL sidecar bytes must be preserved at new dir on checkpoint failure"
    );
    assert_eq!(
        std::fs::read(new_dir.join(format!("{STATE_DB_FILENAME}-shm"))).unwrap(),
        b"shm-bytes",
        "SHM sidecar bytes must be preserved at new dir on checkpoint failure"
    );
    // And removed from legacy (moved, not copied).
    assert!(
        !legacy
            .path()
            .join(format!("{STATE_DB_FILENAME}-wal"))
            .exists()
    );
    assert!(
        !legacy
            .path()
            .join(format!("{STATE_DB_FILENAME}-shm"))
            .exists()
    );
}

// ---------------------------------------------------------------------------
// backup_runs
// ---------------------------------------------------------------------------

fn backup_run_draft(name: &str, artifact: Option<&str>) -> BackupRunDraft {
    BackupRunDraft {
        name: name.to_string(),
        source: "/var/lib/app/data.db".to_string(),
        destination_path: artifact.map(|s| s.to_string()),
        size_bytes: artifact.map(|_| 42),
        status: if artifact.is_some() {
            BackupRunStatus::Success
        } else {
            BackupRunStatus::Failed
        },
        error: artifact.map(|_| None).unwrap_or(Some("boom".to_string())),
        started_at: "2026-08-01T00:00:00Z".to_string(),
        finished_at: "2026-08-01T00:00:01Z".to_string(),
    }
}

#[test]
fn recorded_runs_round_trip_through_the_state_store() {
    let store = StateStore::open_in_memory().expect("store");
    let written = store
        .record_backup_run(&backup_run_draft(
            "db",
            Some("/snapshots/data.db.20260801T000000Z"),
        ))
        .expect("insert");

    assert!(written.id > 0);
    let read_back = store.backup_runs("db").expect("history");
    assert_eq!(read_back.len(), 1);
    let row = &read_back[0];
    assert_eq!(row.id, written.id);
    assert_eq!(row.name, "db");
    assert_eq!(row.source, "/var/lib/app/data.db");
    assert_eq!(
        row.destination_path.as_deref(),
        Some("/snapshots/data.db.20260801T000000Z")
    );
    assert_eq!(row.size_bytes, Some(42));
    assert_eq!(row.status, BackupRunStatus::Success);
    assert_eq!(row.error, None);
    assert_eq!(row.started_at, "2026-08-01T00:00:00Z");
    assert_eq!(row.finished_at, "2026-08-01T00:00:01Z");
}

#[test]
fn a_failed_run_persists_null_artifact_columns() {
    let store = StateStore::open_in_memory().expect("store");
    store
        .record_backup_run(&backup_run_draft("db", None))
        .expect("insert");

    let row = &store.backup_runs("db").expect("history")[0];
    assert_eq!(row.destination_path, None);
    assert_eq!(row.size_bytes, None);
    assert_eq!(row.status, BackupRunStatus::Failed);
    assert_eq!(row.error.as_deref(), Some("boom"));
    assert!(!row.has_artifact());
}

#[test]
fn backup_runs_are_returned_newest_first_and_scoped_by_name() {
    let store = StateStore::open_in_memory().expect("store");
    let a1 = store
        .record_backup_run(&backup_run_draft("alpha", Some("/a1")))
        .expect("insert");
    store
        .record_backup_run(&backup_run_draft("beta", Some("/b1")))
        .expect("insert");
    let a2 = store
        .record_backup_run(&backup_run_draft("alpha", Some("/a2")))
        .expect("insert");

    let alpha = store.backup_runs("alpha").expect("history");
    assert_eq!(
        alpha.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![a2.id, a1.id]
    );
    assert_eq!(store.backup_runs("beta").expect("history").len(), 1);
}

#[test]
fn latest_backup_run_reports_the_newest_or_none() {
    let store = StateStore::open_in_memory().expect("store");
    assert!(store.latest_backup_run("db").expect("query").is_none());

    store
        .record_backup_run(&backup_run_draft("db", Some("/one")))
        .expect("insert");
    let newest = store
        .record_backup_run(&backup_run_draft("db", Some("/two")))
        .expect("insert");

    let latest = store.latest_backup_run("db").expect("query").expect("row");
    assert_eq!(latest.id, newest.id);
    assert_eq!(latest.destination_path.as_deref(), Some("/two"));
}

#[test]
fn delete_backup_run_removes_only_that_row() {
    let store = StateStore::open_in_memory().expect("store");
    let first = store
        .record_backup_run(&backup_run_draft("db", Some("/one")))
        .expect("insert");
    let second = store
        .record_backup_run(&backup_run_draft("db", Some("/two")))
        .expect("insert");

    store.delete_backup_run(first.id).expect("delete");

    let rows = store.backup_runs("db").expect("history");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, second.id);
}

#[test]
fn an_unrecognized_persisted_status_reads_as_failed() {
    let store = StateStore::open_in_memory().expect("store");
    let row = store
        .record_backup_run(&backup_run_draft("db", Some("/one")))
        .expect("insert");
    store
        .conn
        .execute(
            "UPDATE backup_runs SET status = 'nonsense' WHERE id = ?1",
            rusqlite::params![row.id],
        )
        .expect("corrupt the row");

    let read_back = &store.backup_runs("db").expect("history")[0];
    assert_eq!(read_back.status, BackupRunStatus::Failed);
}

// ---------------------------------------------------------------------------
// bootstrapped_managers
// ---------------------------------------------------------------------------

#[test]
fn bootstrap_path_dirs_round_trip_preserves_order() {
    let store = StateStore::open_in_memory().unwrap();
    let dirs = vec![
        "/opt/homebrew/bin".to_string(),
        "/opt/homebrew/sbin".to_string(),
    ];
    store.record_bootstrapped_path_dirs("brew", &dirs).unwrap();

    // The generated env file's content is hashed and compared on every
    // reconcile tick, so a reordered read would be reported as drift forever.
    assert_eq!(
        store.bootstrapped_managers().unwrap(),
        vec![("brew".to_string(), dirs)]
    );
}

#[test]
fn bootstrap_path_dirs_replaces_an_earlier_record_for_the_same_manager() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .record_bootstrapped_path_dirs("brew", &["/usr/local/bin".to_string()])
        .unwrap();
    store
        .record_bootstrapped_path_dirs("brew", &["/opt/homebrew/bin".to_string()])
        .unwrap();

    // A re-bootstrap that lands in a different prefix must not leave the old
    // prefix on PATH alongside the new one.
    assert_eq!(
        store.bootstrapped_managers().unwrap(),
        vec![("brew".to_string(), vec!["/opt/homebrew/bin".to_string()])]
    );
}

#[test]
fn bootstrap_path_dirs_orders_managers_deterministically() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .record_bootstrapped_path_dirs("npm", &["/home/u/.npm-global/bin".to_string()])
        .unwrap();
    store
        .record_bootstrapped_path_dirs("brew", &["/opt/homebrew/bin".to_string()])
        .unwrap();

    let names: Vec<String> = store
        .bootstrapped_managers()
        .unwrap()
        .into_iter()
        .map(|(m, _)| m)
        .collect();
    assert_eq!(
        names,
        vec!["brew".to_string(), "npm".to_string()],
        "insertion order must not leak into the read"
    );
}

#[test]
fn bootstrap_path_dirs_skips_an_undecodable_row() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .record_bootstrapped_path_dirs("brew", &["/opt/homebrew/bin".to_string()])
        .unwrap();
    store
        .conn
        .execute(
            "INSERT INTO bootstrapped_managers (manager, path_dirs, bootstrapped_at)
             VALUES ('npm', 'not-json', '2026-01-01T00:00:00Z')",
            params![],
        )
        .unwrap();

    // One unreadable row must not wedge `cfgd plan`, `cfgd status`, and the
    // daemon tick, all of which read this table.
    assert_eq!(
        store.bootstrapped_managers().unwrap(),
        vec![("brew".to_string(), vec!["/opt/homebrew/bin".to_string()])]
    );
}

// ---------------------------------------------------------------------------
// package_manager_prefixes
// ---------------------------------------------------------------------------

#[test]
fn package_manager_prefix_returns_none_when_nothing_recorded() {
    let store = StateStore::open_in_memory().unwrap();
    assert_eq!(store.package_manager_prefix("npm").unwrap(), None);
}

#[test]
fn package_manager_prefix_round_trips_prefix_and_fallback_flag() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .record_package_manager_prefix("npm", "/home/u/.npm-global", true)
        .unwrap();

    assert_eq!(
        store.package_manager_prefix("npm").unwrap(),
        Some(("/home/u/.npm-global".to_string(), true))
    );
}

#[test]
fn package_manager_prefix_replaces_an_earlier_record_for_the_same_manager() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .record_package_manager_prefix("npm", "/usr/local", false)
        .unwrap();
    store
        .record_package_manager_prefix("npm", "/home/u/.npm-global", true)
        .unwrap();

    // A prefix that was writable on an earlier run but isn't any more must
    // not leave the stale writable-prefix record readable alongside the new
    // fallback one — every later operation resolves through this row.
    assert_eq!(
        store.package_manager_prefix("npm").unwrap(),
        Some(("/home/u/.npm-global".to_string(), true))
    );
}

#[test]
fn package_manager_prefix_is_scoped_per_manager() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .record_package_manager_prefix("npm", "/home/u/.npm-global", true)
        .unwrap();
    store
        .record_package_manager_prefix("pipx", "/home/u/.local/pipx", false)
        .unwrap();

    assert_eq!(
        store.package_manager_prefix("npm").unwrap(),
        Some(("/home/u/.npm-global".to_string(), true))
    );
    assert_eq!(
        store.package_manager_prefix("pipx").unwrap(),
        Some(("/home/u/.local/pipx".to_string(), false))
    );
}

#[test]
fn package_manager_prefix_record_returns_none_when_nothing_recorded() {
    let store = StateStore::open_in_memory().unwrap();
    assert!(
        store
            .package_manager_prefix_record("npm")
            .unwrap()
            .is_none()
    );
}

#[test]
fn package_manager_prefix_record_surfaces_resolved_at() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .record_package_manager_prefix("npm", "/home/u/.npm-global", true)
        .unwrap();

    let record = store
        .package_manager_prefix_record("npm")
        .unwrap()
        .expect("row should exist");
    assert_eq!(record.manager, "npm");
    assert_eq!(record.prefix, "/home/u/.npm-global");
    assert!(record.is_fallback);
    assert!(
        !record.resolved_at.is_empty(),
        "resolved_at must be populated"
    );
}

#[test]
fn forget_package_manager_prefix_returns_none_when_nothing_recorded() {
    let store = StateStore::open_in_memory().unwrap();
    assert!(
        store
            .forget_package_manager_prefix("npm")
            .unwrap()
            .is_none()
    );
}

#[test]
fn forget_package_manager_prefix_deletes_the_row_and_returns_it() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .record_package_manager_prefix("npm", "/home/u/.npm-global", true)
        .unwrap();

    let forgotten = store
        .forget_package_manager_prefix("npm")
        .unwrap()
        .expect("row should have existed");
    assert_eq!(forgotten.prefix, "/home/u/.npm-global");

    // The row must actually be gone, not just returned — the whole point is
    // forcing the next resolution to derive fresh.
    assert_eq!(store.package_manager_prefix("npm").unwrap(), None);
}

#[test]
fn forget_package_manager_prefix_is_scoped_per_manager() {
    let store = StateStore::open_in_memory().unwrap();
    store
        .record_package_manager_prefix("npm", "/home/u/.npm-global", true)
        .unwrap();
    store
        .record_package_manager_prefix("pipx", "/home/u/.local/pipx", false)
        .unwrap();

    store.forget_package_manager_prefix("npm").unwrap();

    assert_eq!(store.package_manager_prefix("npm").unwrap(), None);
    assert_eq!(
        store.package_manager_prefix("pipx").unwrap(),
        Some(("/home/u/.local/pipx".to_string(), false))
    );
}

#[test]
fn journal_entry_is_file_work_covers_module_file_deploys() {
    let entry = |phase: &str, action_type: &str, resource_id: &str| JournalEntry {
        id: 1,
        apply_id: 1,
        action_index: 0,
        phase: phase.to_string(),
        action_type: action_type.to_string(),
        resource_id: resource_id.to_string(),
        pre_state: None,
        post_state: None,
        status: "success".to_string(),
        error: None,
        started_at: String::new(),
        completed_at: None,
        script_output: None,
    };

    // The three original disjuncts.
    assert!(entry("files", "file", "~/.gitconfig").is_file_work());
    assert!(entry("modules", "file", "~/.tmux.conf").is_file_work());
    assert!(entry("modules", "unknown", "file:~/.vimrc").is_file_work());

    // Module file deploys journal as module:<name>:files:<n> — action_type
    // "module", id "<name>:files:<n>". Their writes are restored from
    // file_backups, so rollback must not list them as unrecoverable.
    assert!(entry("modules", "module", "nvim:files:3").is_file_work());

    // Other module verbs stay non-file work.
    assert!(!entry("modules", "module", "nvim:script").is_file_work());
    assert!(!entry("modules", "module", "nvim:skip").is_file_work());
    assert!(!entry("modules", "module", "nvim:packages:fd,rg").is_file_work());
    assert!(!entry("packages", "package", "apt:install:sl").is_file_work());

    // Env write/inject rows journal as action_type "env" with the target
    // path as the id (the write/inject verb is dropped by the two-colon
    // parse). Their pre-states are captured through `action_target_path`
    // into file_backups, so rollback restores them and must not list them
    // as unrecoverable.
    assert!(entry("env", "env", "/home/u/.cfgd.env").is_file_work());
    assert!(entry("env", "env", "~/.bashrc").is_file_work());

    // The one env row with no file behind it: the live-session refresh
    // (`env:session:refresh` parses to id "refresh"). Session-manager state
    // has no backup, so it stays in the unrecoverable report.
    assert!(!entry("env", "env", "refresh").is_file_work());
}

#[test]
fn is_file_work_classifies_by_resource_identity_not_phase() {
    let entry = |phase: &str, action_type: &str, resource_id: &str| JournalEntry {
        id: 1,
        apply_id: 1,
        action_index: 0,
        phase: phase.to_string(),
        action_type: action_type.to_string(),
        resource_id: resource_id.to_string(),
        pre_state: None,
        post_state: None,
        status: "success".to_string(),
        error: None,
        started_at: String::new(),
        completed_at: None,
        script_output: None,
    };

    // A module's encryption/strategy skip is planned into the `files` phase and
    // writes nothing. Under a phase term it would be reported as restorable
    // file work and rollback would claim to have undone a write that never
    // happened.
    assert!(!entry("files", "module", "nvim:skip").is_file_work());
    assert!(!entry("files", "package", "brew:install:fd").is_file_work());
    assert!(!entry("files", "script", "post:setup.sh").is_file_work());

    // The identity terms answer the same in the re-routed phase as they did in
    // `modules`, which is the whole point of keying on identity.
    assert!(entry("files", "module", "nvim:files:3").is_file_work());
    assert!(entry("packages", "file", "~/.gitconfig").is_file_work());
    assert!(entry("post-scripts", "unknown", "file:~/.vimrc").is_file_work());
}
