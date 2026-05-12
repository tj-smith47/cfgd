use super::*;
use crate::gateway::test_state::test_db;

fn future_expiry() -> String {
    cfgd_core::unix_secs_to_iso8601(cfgd_core::unix_secs_now() + 3600)
}

fn past_expiry() -> String {
    cfgd_core::unix_secs_to_iso8601(cfgd_core::unix_secs_now().saturating_sub(3600))
}

#[tokio::test(flavor = "current_thread")]
async fn register_and_get_device() {
    let (db, _tmp) = test_db();
    let device = db
        .register_device("dev-1", "workstation-1", "linux", "x86_64", "abc123", None)
        .await
        .expect("register failed");
    assert_eq!(device.id, "dev-1");
    assert_eq!(device.hostname, "workstation-1");
    assert_eq!(device.status, DeviceStatus::Healthy);
    assert!(device.desired_config.is_none());

    let fetched = db.get_device("dev-1").await.expect("get failed");
    assert_eq!(fetched.hostname, "workstation-1");
}

#[tokio::test(flavor = "current_thread")]
async fn list_devices_empty() {
    let (db, _tmp) = test_db();
    let devices = db.list_devices().await.expect("list failed");
    assert!(devices.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn update_checkin() {
    let (db, _tmp) = test_db();
    db.register_device("dev-1", "ws-1", "linux", "x86_64", "hash1", None)
        .await
        .expect("register failed");
    db.update_checkin("dev-1", "hash2", None)
        .await
        .expect("update failed");
    let device = db.get_device("dev-1").await.expect("get failed");
    assert_eq!(device.config_hash, "hash2");
}

#[tokio::test(flavor = "current_thread")]
async fn update_checkin_not_found() {
    let (db, _tmp) = test_db();
    let result = db.update_checkin("nonexistent", "hash", None).await;
    let err = result.unwrap_err();
    assert!(
        matches!(err, GatewayError::NotFound(_)),
        "expected NotFound, got: {err}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn drift_events() {
    let (db, _tmp) = test_db();
    db.register_device("dev-1", "ws-1", "linux", "x86_64", "hash1", None)
        .await
        .expect("register failed");
    let event = db
        .record_drift_event("dev-1", "package vim drifted")
        .await
        .expect("record failed");
    assert_eq!(event.device_id, "dev-1");

    let device = db.get_device("dev-1").await.expect("get failed");
    assert_eq!(device.status, DeviceStatus::Drifted);

    let events = db.list_drift_events("dev-1").await.expect("list failed");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].details, "package vim drifted");
}

#[tokio::test(flavor = "current_thread")]
async fn drift_event_for_nonexistent_device() {
    let (db, _tmp) = test_db();
    let result = db.record_drift_event("nonexistent", "details").await;
    let err = result.unwrap_err();
    assert!(
        matches!(err, GatewayError::NotFound(_)),
        "expected NotFound, got: {err}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn register_device_upsert() {
    let (db, _tmp) = test_db();
    db.register_device("dev-1", "ws-1", "linux", "x86_64", "hash1", None)
        .await
        .expect("first register");
    let device = db
        .register_device("dev-1", "ws-1-renamed", "linux", "x86_64", "hash2", None)
        .await
        .expect("second register");
    assert_eq!(device.hostname, "ws-1-renamed");
    assert_eq!(device.config_hash, "hash2");

    let all = db.list_devices().await.expect("list");
    assert_eq!(all.len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn set_and_get_device_config() {
    let (db, _tmp) = test_db();
    db.register_device("dev-1", "ws-1", "linux", "x86_64", "hash1", None)
        .await
        .expect("register failed");

    let config = serde_json::json!({"packages": ["vim", "git"]});
    db.set_device_config("dev-1", &config)
        .await
        .expect("set config failed");

    let device = db.get_device("dev-1").await.expect("get failed");
    assert_eq!(device.desired_config, Some(config));
}

#[tokio::test(flavor = "current_thread")]
async fn set_config_not_found() {
    let (db, _tmp) = test_db();
    let config = serde_json::json!({"packages": []});
    let result = db.set_device_config("nonexistent", &config).await;
    let err = result.unwrap_err();
    assert!(
        matches!(err, GatewayError::NotFound(_)),
        "expected NotFound, got: {err}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn record_and_list_checkin_events() {
    let (db, _tmp) = test_db();
    db.register_device("dev-1", "ws-1", "linux", "x86_64", "hash1", None)
        .await
        .expect("register failed");

    let event = db
        .record_checkin("dev-1", "hash1", false)
        .await
        .expect("record checkin failed");
    assert_eq!(event.device_id, "dev-1");
    assert!(!event.config_changed);

    db.record_checkin("dev-1", "hash2", true)
        .await
        .expect("record checkin 2 failed");

    let events = db
        .list_checkin_events("dev-1")
        .await
        .expect("list checkin events failed");
    assert_eq!(events.len(), 2);
    let changed_count = events.iter().filter(|e| e.config_changed).count();
    assert_eq!(changed_count, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn list_checkin_events_nonexistent_device() {
    let (db, _tmp) = test_db();
    let result = db.list_checkin_events("nonexistent").await;
    let err = result.unwrap_err();
    assert!(
        matches!(err, GatewayError::NotFound(_)),
        "expected NotFound, got: {err}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn fleet_events_combined() {
    let (db, _tmp) = test_db();
    db.register_device("dev-1", "ws-1", "linux", "x86_64", "hash1", None)
        .await
        .expect("register failed");

    db.record_checkin("dev-1", "hash1", false)
        .await
        .expect("checkin failed");
    db.record_drift_event(
        "dev-1",
        r#"[{"field":"pkg","expected":"vim","actual":"missing"}]"#,
    )
    .await
    .expect("drift failed");
    db.record_checkin("dev-1", "hash2", true)
        .await
        .expect("checkin 2 failed");

    let events = db
        .list_fleet_events(100)
        .await
        .expect("list fleet events failed");
    assert_eq!(events.len(), 3);

    let types: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
    assert!(types.contains(&"drift"));
    assert!(types.contains(&"checkin") || types.contains(&"config-changed"));
}

#[tokio::test(flavor = "current_thread")]
async fn fleet_events_empty() {
    let (db, _tmp) = test_db();
    let events = db
        .list_fleet_events(100)
        .await
        .expect("list fleet events failed");
    assert!(events.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn force_reconcile() {
    let (db, _tmp) = test_db();
    db.register_device("dev-1", "ws-1", "linux", "x86_64", "hash1", None)
        .await
        .expect("register failed");

    db.set_force_reconcile("dev-1")
        .await
        .expect("force reconcile failed");

    let device = db.get_device("dev-1").await.expect("get failed");
    assert_eq!(device.status, DeviceStatus::PendingReconcile);
}

#[tokio::test(flavor = "current_thread")]
async fn force_reconcile_not_found() {
    let (db, _tmp) = test_db();
    let result = db.set_force_reconcile("nonexistent").await;
    let err = result.unwrap_err();
    assert!(
        matches!(err, GatewayError::NotFound(_)),
        "expected NotFound, got: {err}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn compliance_summary_stored_on_register() {
    let (db, _tmp) = test_db();
    let summary = serde_json::json!({"compliant": 5, "warning": 1, "violation": 0});
    let device = db
        .register_device("dev-c", "ws-c", "linux", "x86_64", "hash1", Some(&summary))
        .await
        .expect("register failed");
    assert_eq!(device.compliance_summary, Some(summary));
}

#[tokio::test(flavor = "current_thread")]
async fn compliance_summary_stored_on_checkin_update() {
    let (db, _tmp) = test_db();
    db.register_device("dev-c2", "ws-c2", "linux", "x86_64", "hash1", None)
        .await
        .expect("register failed");

    let summary = serde_json::json!({"compliant": 10, "warning": 0, "violation": 2});
    db.update_checkin("dev-c2", "hash2", Some(&summary))
        .await
        .expect("update failed");

    let device = db.get_device("dev-c2").await.expect("get failed");
    assert_eq!(device.config_hash, "hash2");
    assert_eq!(device.compliance_summary, Some(summary));
}

#[tokio::test(flavor = "current_thread")]
async fn compliance_summary_null_when_not_provided() {
    let (db, _tmp) = test_db();
    let device = db
        .register_device("dev-c3", "ws-c3", "linux", "x86_64", "hash1", None)
        .await
        .expect("register failed");
    assert!(device.compliance_summary.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn create_and_consume_bootstrap_token() {
    let (db, _tmp) = test_db();
    let expires_at = future_expiry();

    let token = db
        .create_bootstrap_token("hash123", "jdoe", Some("acme"), &expires_at)
        .await
        .expect("create failed");
    assert_eq!(token.username, "jdoe");
    assert_eq!(token.team.as_deref(), Some("acme"));
    assert!(token.used_at.is_none());

    let consumed = db
        .validate_and_consume_bootstrap_token("hash123", "dev-1")
        .await
        .expect("validate_and_consume failed");
    assert_eq!(consumed.username, "jdoe");
    assert!(consumed.used_at.is_some());
    assert_eq!(consumed.used_by_device.as_deref(), Some("dev-1"));
}

#[tokio::test(flavor = "current_thread")]
async fn consume_expired_token_fails() {
    let (db, _tmp) = test_db();
    let expires_at = past_expiry();

    db.create_bootstrap_token("hash_expired", "jdoe", None, &expires_at)
        .await
        .expect("create failed");

    let result = db
        .validate_and_consume_bootstrap_token("hash_expired", "dev-1")
        .await;
    let err = result.unwrap_err();
    assert!(
        matches!(err, GatewayError::InvalidRequest(_)),
        "expected InvalidRequest, got: {err}"
    );
    assert!(
        err.to_string().contains("expired"),
        "expected 'expired' in message, got: {err}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn consume_used_token_fails() {
    let (db, _tmp) = test_db();
    let expires_at = future_expiry();

    db.create_bootstrap_token("hash_used", "jdoe", None, &expires_at)
        .await
        .expect("create failed");
    db.validate_and_consume_bootstrap_token("hash_used", "dev-1")
        .await
        .expect("first consume succeeded");

    let result = db
        .validate_and_consume_bootstrap_token("hash_used", "dev-2")
        .await;
    let err = result.unwrap_err();
    assert!(
        matches!(err, GatewayError::InvalidRequest(_)),
        "expected InvalidRequest, got: {err}"
    );
    assert!(
        err.to_string().contains("already been used"),
        "expected 'already been used' in message, got: {err}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn consume_nonexistent_token_fails() {
    let (db, _tmp) = test_db();
    let result = db
        .validate_and_consume_bootstrap_token("no_such_hash", "dev-1")
        .await;
    let err = result.unwrap_err();
    assert!(
        matches!(err, GatewayError::Unauthorized),
        "expected Unauthorized, got: {err}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn list_bootstrap_tokens() {
    let (db, _tmp) = test_db();
    let expires_at = future_expiry();

    db.create_bootstrap_token("h1", "user1", None, &expires_at)
        .await
        .expect("create 1 failed");
    db.create_bootstrap_token("h2", "user2", Some("team"), &expires_at)
        .await
        .expect("create 2 failed");

    let tokens = db.list_bootstrap_tokens().await.expect("list failed");
    assert_eq!(tokens.len(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn delete_bootstrap_token() {
    let (db, _tmp) = test_db();
    let expires_at = future_expiry();

    let token = db
        .create_bootstrap_token("h_del", "jdoe", None, &expires_at)
        .await
        .expect("create failed");

    db.delete_bootstrap_token(&token.id)
        .await
        .expect("delete failed");

    let tokens = db.list_bootstrap_tokens().await.expect("list failed");
    assert!(tokens.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn delete_nonexistent_token_fails() {
    let (db, _tmp) = test_db();
    let result = db.delete_bootstrap_token("no-such-id").await;
    let err = result.unwrap_err();
    assert!(
        matches!(err, GatewayError::NotFound(_)),
        "expected NotFound, got: {err}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn create_and_validate_device_credential() {
    let (db, _tmp) = test_db();

    db.create_device_credential("dev-1", "keyhash1", "jdoe", Some("acme"))
        .await
        .expect("create failed");

    let cred = db
        .validate_device_credential("keyhash1")
        .await
        .expect("validate failed");
    assert_eq!(cred.device_id, "dev-1");
    assert_eq!(cred.username, "jdoe");
    assert_eq!(cred.team.as_deref(), Some("acme"));
    assert!(!cred.revoked);
}

#[tokio::test(flavor = "current_thread")]
async fn validate_nonexistent_credential_fails() {
    let (db, _tmp) = test_db();
    let result = db.validate_device_credential("no_such_hash").await;
    let err = result.unwrap_err();
    assert!(
        matches!(err, GatewayError::Unauthorized),
        "expected Unauthorized, got: {err}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn revoked_credential_fails_validation() {
    let (db, _tmp) = test_db();

    db.create_device_credential("dev-1", "keyhash_rev", "jdoe", None)
        .await
        .expect("create failed");
    db.revoke_device_credential("dev-1")
        .await
        .expect("revoke failed");

    let result = db.validate_device_credential("keyhash_rev").await;
    let err = result.unwrap_err();
    assert!(
        matches!(err, GatewayError::Unauthorized),
        "expected Unauthorized, got: {err}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn credential_upsert_on_re_enrollment() {
    let (db, _tmp) = test_db();

    db.create_device_credential("dev-1", "key_old", "jdoe", None)
        .await
        .expect("create 1 failed");
    db.create_device_credential("dev-1", "key_new", "jdoe", Some("newteam"))
        .await
        .expect("create 2 (upsert) failed");

    let result = db.validate_device_credential("key_old").await;
    let err = result.unwrap_err();
    assert!(
        matches!(err, GatewayError::Unauthorized),
        "expected Unauthorized, got: {err}"
    );

    let cred = db
        .validate_device_credential("key_new")
        .await
        .expect("validate new failed");
    assert_eq!(cred.device_id, "dev-1");
    assert_eq!(cred.team.as_deref(), Some("newteam"));
}

#[tokio::test(flavor = "current_thread")]
async fn revoke_nonexistent_credential_fails() {
    let (db, _tmp) = test_db();
    let result = db.revoke_device_credential("no-such-device").await;
    let err = result.unwrap_err();
    assert!(
        matches!(err, GatewayError::NotFound(_)),
        "expected NotFound, got: {err}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn add_and_list_user_public_keys() {
    let (db, _tmp) = test_db();

    let key = db
        .add_user_public_key(
            "jdoe",
            "ssh",
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIG... jdoe@work",
            "SHA256:abc123def456",
            Some("work laptop"),
        )
        .await
        .expect("add key failed");
    assert_eq!(key.username, "jdoe");
    assert_eq!(key.key_type, "ssh");
    assert_eq!(key.label.as_deref(), Some("work laptop"));

    db.add_user_public_key(
        "jdoe",
        "gpg",
        "-----BEGIN PGP PUBLIC KEY BLOCK-----\n...\n-----END PGP PUBLIC KEY BLOCK-----",
        "ABCD1234EFGH5678",
        None,
    )
    .await
    .expect("add gpg key failed");

    let keys = db.list_user_public_keys("jdoe").await.expect("list failed");
    assert_eq!(keys.len(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn list_user_public_keys_empty() {
    let (db, _tmp) = test_db();
    let keys = db
        .list_user_public_keys("nobody")
        .await
        .expect("list failed");
    assert!(keys.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn delete_user_public_key() {
    let (db, _tmp) = test_db();

    let key = db
        .add_user_public_key("jdoe", "ssh", "ssh-ed25519 AAAA...", "SHA256:xyz", None)
        .await
        .expect("add failed");

    db.delete_user_public_key(&key.id)
        .await
        .expect("delete failed");

    let keys = db.list_user_public_keys("jdoe").await.expect("list failed");
    assert!(keys.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn delete_nonexistent_public_key_fails() {
    let (db, _tmp) = test_db();
    let result = db.delete_user_public_key("no-such-id").await;
    let err = result.unwrap_err();
    assert!(
        matches!(err, GatewayError::NotFound(_)),
        "expected NotFound, got: {err}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn create_and_consume_enrollment_challenge() {
    let (db, _tmp) = test_db();

    let challenge = db
        .create_enrollment_challenge(
            "jdoe",
            "dev-1",
            "macbook",
            ("macos", "aarch64"),
            "random-nonce-abc",
            300,
        )
        .await
        .expect("create challenge failed");
    assert_eq!(challenge.username, "jdoe");
    assert_eq!(challenge.nonce, "random-nonce-abc");

    let consumed = db
        .consume_enrollment_challenge(&challenge.id)
        .await
        .expect("consume failed");
    assert_eq!(consumed.username, "jdoe");
    assert_eq!(consumed.device_id, "dev-1");
}

#[tokio::test(flavor = "current_thread")]
async fn consume_challenge_twice_fails() {
    let (db, _tmp) = test_db();

    let challenge = db
        .create_enrollment_challenge(
            "jdoe",
            "dev-1",
            "macbook",
            ("macos", "aarch64"),
            "nonce1",
            300,
        )
        .await
        .expect("create failed");

    db.consume_enrollment_challenge(&challenge.id)
        .await
        .expect("first consume ok");

    let result = db.consume_enrollment_challenge(&challenge.id).await;
    let err = result.unwrap_err();
    assert!(
        matches!(err, GatewayError::InvalidRequest(_)),
        "expected InvalidRequest, got: {err}"
    );
    assert!(
        err.to_string().contains("already been used"),
        "expected 'already been used' in message, got: {err}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn consume_expired_challenge_fails() {
    let (db, _tmp) = test_db();

    // Insert a challenge with an already-past expires_at via the writer tx.
    let id = uuid::Uuid::new_v4().to_string();
    let past = past_expiry();
    let id_c = id.clone();
    let past_c = past.clone();
    db.with_write_tx(move |tx| {
            tx.execute(
                "INSERT INTO enrollment_challenges (id, username, device_id, hostname, os, arch, nonce, created_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![&id_c, "jdoe", "dev-1", "macbook", "macos", "aarch64", "nonce2", &past_c, &past_c],
            )?;
            Ok(())
        })
        .await
        .expect("insert failed");

    let result = db.consume_enrollment_challenge(&id).await;
    let err = result.unwrap_err();
    assert!(
        matches!(err, GatewayError::InvalidRequest(_)),
        "expected InvalidRequest, got: {err}"
    );
    assert!(
        err.to_string().contains("expired"),
        "expected 'expired' in message, got: {err}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn consume_nonexistent_challenge_fails() {
    let (db, _tmp) = test_db();
    let result = db.consume_enrollment_challenge("no-such-id").await;
    let err = result.unwrap_err();
    assert!(
        matches!(err, GatewayError::NotFound(_)),
        "expected NotFound, got: {err}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn schema_version_is_set() {
    let (db, _tmp) = test_db();
    assert_eq!(db.schema_version_blocking(), MIGRATIONS.len());
}

#[tokio::test(flavor = "current_thread")]
async fn migration_upgrades_v1_database() {
    // Simulate a schema at version 1: create tables without desired_config.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("upgrade-test.db");
    let path_str = path.to_str().unwrap();

    {
        let conn = Connection::open(path_str).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .unwrap();
        conn.execute_batch(
            "CREATE TABLE devices (
                    id TEXT PRIMARY KEY,
                    hostname TEXT NOT NULL,
                    os TEXT NOT NULL,
                    arch TEXT NOT NULL,
                    last_checkin TEXT NOT NULL,
                    config_hash TEXT NOT NULL,
                    status TEXT NOT NULL
                );
                CREATE TABLE schema_version (version INTEGER NOT NULL);
                INSERT INTO schema_version (version) VALUES (1);",
        )
        .unwrap();
        conn.execute(
                "INSERT INTO devices (id, hostname, os, arch, last_checkin, config_hash, status) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params!["dev-1", "host-1", "linux", "amd64", "2026-01-01T00:00:00Z", "abc", "healthy"],
            ).unwrap();
    }

    let db = ServerDb::open(path_str).unwrap();
    assert_eq!(db.schema_version_blocking(), MIGRATIONS.len());

    let device = db.get_device("dev-1").await.unwrap();
    assert_eq!(device.hostname, "host-1");
    assert!(device.desired_config.is_none());
}

// --- Pool refactor guarantee tests (B1 W-6) ---

/// Enrollment is wrapped in `with_write_tx`; any step failing inside the
/// closure must roll back the whole transaction, including the bootstrap
/// token's used_at flag. Regression guard: the pre-pool-refactor enroll
/// handler hand-rolled BEGIN IMMEDIATE/COMMIT/ROLLBACK and was easy to
/// get wrong.
#[tokio::test(flavor = "current_thread")]
async fn enroll_transaction_rolls_back_on_credential_failure() {
    let (db, _tmp) = test_db();

    let expires_at = future_expiry();
    let token = db
        .create_bootstrap_token("token-hash-xyz", "alice", None, &expires_at)
        .await
        .expect("create bootstrap token");

    // Pre-seed a credential holding api_key_hash = "collide" so the
    // credential insert inside the tx fails on UNIQUE(api_key_hash).
    db.register_device("dev-existing", "h", "linux", "x86_64", "x", None)
        .await
        .expect("register existing device");
    db.create_device_credential("dev-existing", "collide", "alice", None)
        .await
        .expect("seed credential");

    let res = db
        .with_write_tx(move |tx| {
            validate_and_consume_bootstrap_token_tx(tx, "token-hash-xyz", "dev-new")?;
            register_device_tx(tx, "dev-new", "h", "linux", "x86_64", "x", None)?;
            // UNIQUE(api_key_hash) on "collide" is already taken → error.
            create_device_credential_tx(tx, "dev-new", "collide", "alice", None)?;
            Ok(())
        })
        .await;
    assert!(res.is_err(), "expected tx failure");

    // Bootstrap token must still be unconsumed.
    let tokens = db.list_bootstrap_tokens().await.expect("list tokens");
    let t = tokens
        .iter()
        .find(|t| t.id == token.id)
        .expect("token still exists");
    assert!(
        t.used_at.is_none(),
        "token should NOT be consumed after tx rollback"
    );
    assert!(t.used_by_device.is_none());

    // The new device must not exist.
    let err = db
        .get_device("dev-new")
        .await
        .expect_err("dev-new should not exist after rollback");
    assert!(matches!(err, GatewayError::NotFound(_)));
}

/// Checkin was non-transactional pre-refactor (get/update/record_checkin
/// ran on a shared mutex but as three separate ops). Regression guard:
/// in the new handler, a failure in record_checkin must roll back the
/// preceding update_checkin so the device's config_hash stays consistent.
#[tokio::test(flavor = "current_thread")]
async fn checkin_is_atomic_across_get_update_record() {
    let (db, _tmp) = test_db();
    db.register_device("d", "h", "linux", "x86_64", "hash-1", None)
        .await
        .expect("register");

    let res = db
        .with_write_tx(move |tx| {
            update_checkin_tx(tx, "d", "hash-2", None)?;
            // FK violation: device_id must exist in devices.
            record_checkin_tx(tx, "nonexistent-device", "hash-2", true)?;
            Ok(())
        })
        .await;
    assert!(res.is_err(), "expected tx failure on FK");

    let d = db.get_device("d").await.expect("get device");
    assert_eq!(
        d.config_hash, "hash-1",
        "update_checkin must have rolled back"
    );
}

/// With the split-pool model, N concurrent readers should run truly in
/// parallel (WAL supports unlimited concurrent readers) and should NOT
/// serialize a concurrent writer. Pre-refactor, every call serialized
/// on a single `tokio::sync::Mutex<ServerDb>`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_readers_do_not_block_writer() {
    let (db, _tmp) = test_db();
    for i in 0..50 {
        db.register_device(&format!("d{i}"), "h", "linux", "x86_64", "x", None)
            .await
            .expect("seed");
    }

    // Baseline: time a single writer op with no contention.
    let t0 = std::time::Instant::now();
    db.register_device("baseline", "h", "linux", "x86_64", "x", None)
        .await
        .expect("baseline write");
    let baseline = t0.elapsed();

    // Launch 16 concurrent reader loops.
    let mut reader_handles = Vec::new();
    for i in 0..16 {
        let db_c = db.clone();
        reader_handles.push(tokio::spawn(async move {
            for _ in 0..10 {
                let _ = db_c.get_device(&format!("d{}", i % 50)).await;
            }
        }));
    }

    // Give readers a moment to saturate the reader pool.
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;

    // Time a writer while readers are active.
    let t_write = std::time::Instant::now();
    db.register_device("contended", "h", "linux", "x86_64", "x", None)
        .await
        .expect("contended write");
    let write_elapsed = t_write.elapsed();

    for h in reader_handles {
        h.await.expect("reader join");
    }

    // Allow up to 10× baseline as a lenient bound — anything worse would
    // mean readers are blocking the writer (the very bug this removes).
    // In practice this runs well under 3× on a reasonable machine.
    assert!(
        write_elapsed < baseline * 10 + std::time::Duration::from_millis(50),
        "writer took {write_elapsed:?}, baseline {baseline:?} — readers are blocking writer"
    );
}

/// `writer_wait_seconds` histogram must record observations when the
/// writer mutex is contended. Guards against metric wiring regressions
/// that would silently break operator observability of pool pressure.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn writer_contention_metric_observes() {
    use prometheus_client::registry::Registry;

    let mut reg = Registry::default();
    let metrics = crate::metrics::Metrics::new(&mut reg);

    let (db, _tmp) = test_db();
    let db = db.with_metrics(Some(metrics));

    // Four concurrent writers forces the writer mutex to queue at least
    // three of them.
    let mut handles = Vec::new();
    for i in 0..4 {
        let db_c = db.clone();
        handles.push(tokio::spawn(async move {
            db_c.register_device(&format!("c{i}"), "h", "linux", "x86_64", "x", None)
                .await
        }));
    }
    for h in handles {
        h.await.expect("join").expect("write");
    }

    let mut out = String::new();
    prometheus_client::encoding::text::encode(&mut out, &reg).expect("encode");
    // _count line format: `cfgd_operator_gateway_db_writer_wait_seconds_count N`.
    let count_line = out
        .lines()
        .find(|l| l.starts_with("cfgd_operator_gateway_db_writer_wait_seconds_count"))
        .expect("writer_wait_seconds_count present");
    let count: u64 = count_line
        .split_whitespace()
        .last()
        .and_then(|n| n.parse().ok())
        .expect("parse count");
    assert!(count >= 4, "expected ≥4 observations, got {count}:\n{out}");
}

/// When the reader pool is saturated, `pool.get()` must time out and
/// surface as `GatewayError::PoolExhausted` (not a silent hang, not 500).
/// This is the HTTP 503 load-shedding path.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pool_timeout_surfaces_as_pool_exhausted() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("pool.db");
    let db = ServerDb::open_with_config(
        path.to_str().expect("utf8"),
        1,
        std::time::Duration::from_millis(50),
    )
    .expect("open");
    db.register_device("d", "h", "linux", "x86_64", "x", None)
        .await
        .expect("register");

    // Hold the single reader in a blocking sleep inside a read tx.
    let db_hold = db.clone();
    let hold = tokio::spawn(async move {
        db_hold
            .with_read_tx(|_tx| {
                std::thread::sleep(std::time::Duration::from_millis(400));
                Ok(())
            })
            .await
    });

    // Give the holder time to acquire.
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;

    // Second reader must time out on pool acquisition.
    let r = db.get_device("d").await;
    match r {
        Err(GatewayError::PoolExhausted(_)) => {}
        other => panic!("expected PoolExhausted, got {other:?}"),
    }

    hold.await.expect("hold join").expect("hold inner");
}
