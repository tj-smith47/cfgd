use super::*;

// --- Record drift for a specific file ---

pub(crate) fn record_file_drift_to(store: &StateStore, path: &Path) -> bool {
    // POSIX-fold the id so it UPSERT-matches the reconcile path's row, which
    // derives the same file's id via `to_posix_string` (native separators on
    // Windows would otherwise produce a divergent duplicate row).
    match store.record_drift(
        "file",
        &crate::to_posix_string(path),
        None,
        Some("modified"),
        "local",
    ) {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!(error = %e, "failed to record drift");
            false
        }
    }
}

/// Whether a changed `path` is a managed TARGET (one the profile declares),
/// as opposed to a config/source/`.git` path that merely triggers a reconcile.
///
/// Membership is exact: the file watcher also watches the PARENT dir of a
/// not-yet-existing managed file, so sibling files in that parent fire events
/// too — those are not managed targets and must not record drift.
pub(crate) fn path_is_managed_target(path: &Path, managed_paths: &[PathBuf]) -> bool {
    managed_paths.iter().any(|m| m == path)
}

/// Current count of outstanding (unresolved) drift rows. The in-memory
/// `DaemonState.drift_count` is set from this so `/status` matches the `/drift`
/// DB view instead of drifting via an append-only accumulator.
///
/// Returns `None` on a read failure so callers leave the prior count untouched
/// rather than overwriting it with a misleading 0 ("no drift") on a transient
/// DB error.
pub(crate) fn current_drift_count(store: &StateStore) -> Option<u32> {
    match store.unresolved_drift() {
        Ok(events) => Some(events.len() as u32),
        Err(e) => {
            tracing::warn!(error = %e, "cannot read unresolved drift count");
            None
        }
    }
}
