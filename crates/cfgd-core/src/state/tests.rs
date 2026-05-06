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

    let completed = store.journal_completed_actions(apply_id).unwrap();
    assert_eq!(completed.len(), 2);
    assert_eq!(completed[0].resource_id, "/home/user/.bashrc");
    assert_eq!(completed[0].status, "completed");
    assert!(completed[0].script_output.is_none());
    assert_eq!(completed[1].resource_id, "setup.sh");
    assert_eq!(
        completed[1].script_output.as_deref(),
        Some("installed deps\nall good")
    );

    // journal_entries returns all entries including failed ones
    let all = store.journal_entries(apply_id).unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(all[1].status, "failed");
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
fn schema_version_is_4_after_migration() {
    let store = StateStore::open_in_memory().unwrap();
    let version = store.schema_version();
    assert_eq!(version, 4);
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

    let json = serde_json::to_string(&snapshot).unwrap();
    let hash = crate::sha256_hex(json.as_bytes());

    store.store_compliance_snapshot(&snapshot, &hash).unwrap();

    // Retrieve by latest hash
    let latest = store.latest_compliance_hash().unwrap().unwrap();
    assert_eq!(latest, hash);

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
    store.store_compliance_snapshot(&s1, "hash1").unwrap();

    let mut s2 = make_test_snapshot();
    s2.timestamp = "2026-01-02T00:00:00Z".into();
    store.store_compliance_snapshot(&s2, "hash2").unwrap();

    let latest = store.latest_compliance_hash().unwrap().unwrap();
    assert_eq!(latest, "hash2");
}

#[test]
fn compliance_prune_removes_old_snapshots() {
    let store = StateStore::open_in_memory().unwrap();

    let mut s1 = make_test_snapshot();
    s1.timestamp = "2026-01-01T00:00:00Z".into();
    store.store_compliance_snapshot(&s1, "hash1").unwrap();

    let mut s2 = make_test_snapshot();
    s2.timestamp = "2026-01-15T00:00:00Z".into();
    store.store_compliance_snapshot(&s2, "hash2").unwrap();

    let mut s3 = make_test_snapshot();
    s3.timestamp = "2026-02-01T00:00:00Z".into();
    store.store_compliance_snapshot(&s3, "hash3").unwrap();

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
    store.store_compliance_snapshot(&s1, "h1").unwrap();

    let mut s2 = make_test_snapshot();
    s2.timestamp = "2026-01-10T00:00:00Z".into();
    store.store_compliance_snapshot(&s2, "h2").unwrap();

    let mut s3 = make_test_snapshot();
    s3.timestamp = "2026-01-20T00:00:00Z".into();
    store.store_compliance_snapshot(&s3, "h3").unwrap();

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
