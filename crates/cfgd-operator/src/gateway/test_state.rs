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
            event_tx,
            enrollment_method: EnrollmentMethod::Token,
            metrics: None,
            web_sessions: WebSessions::new(),
        },
        tmp,
    )
}
