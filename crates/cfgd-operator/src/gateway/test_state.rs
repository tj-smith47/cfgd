//! Shared `SharedState` builder for gateway tests (web + api).
//!
//! Each test gets a fresh tempdir-backed SQLite DB so tests don't share
//! state. The TempDir handle is returned to the caller so the temp files
//! are cleaned up when the test finishes.
#![cfg(test)]

use crate::gateway::api::{AppState, EnrollmentMethod, SharedState, WebSessions};
use crate::gateway::db::ServerDb;

/// Build a fresh `SharedState` backed by a tempdir SQLite DB.
/// Returns the state plus the tempdir guard — keep the guard alive in the
/// test or the underlying DB file gets deleted out from under you.
pub(crate) fn test_state() -> (SharedState, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("test.db");
    let db = ServerDb::open(path.to_str().expect("utf8")).expect("open db");
    let (event_tx, _) = tokio::sync::broadcast::channel(16);
    (
        AppState {
            db,
            kube_client: None,
            backup_policies: Default::default(),
            event_tx,
            enrollment_method: EnrollmentMethod::Token,
            metrics: None,
            web_sessions: WebSessions::new(),
        },
        tmp,
    )
}

/// The same, holding a kube client the caller supplies — the shape a gateway
/// deployed inside a cluster runs in, where a check-in reaches Kubernetes.
///
/// Pair it with `controllers::test_kube_harness::MockKubeHarness`, whose
/// context carries the client the harness drives.
pub(crate) fn test_state_with_kube(client: kube::Client) -> (SharedState, tempfile::TempDir) {
    let (mut state, tmp) = test_state();
    state.kube_client = Some(client);
    (state, tmp)
}

/// Build a fresh `ServerDb` backed by a tempdir SQLite file.
/// Returns the db plus the tempdir guard — keep the guard alive in the
/// test or the underlying DB file gets deleted out from under you.
pub(crate) fn test_db() -> (ServerDb, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("test.db");
    let db = ServerDb::open(path.to_str().expect("utf8")).expect("open db");
    (db, tmp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn test_db_returns_working_handle() {
        let (db, _tmp) = test_db();
        let devices = db.list_devices().await.expect("list_devices on fresh db");
        assert!(devices.is_empty());
    }
}
