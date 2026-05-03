use super::*;

// --- Record drift for a specific file ---

pub(crate) fn record_file_drift_to(store: &StateStore, path: &Path) -> bool {
    match store.record_drift(
        "file",
        &path.display().to_string(),
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

pub(crate) fn record_file_drift(path: &Path) -> bool {
    let store = match StateStore::open_default() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "cannot open state store for drift recording");
            return false;
        }
    };
    record_file_drift_to(&store, path)
}
