//! Sources lockfile — records the resolved commit SHA for each source so
//! composition is bit-reproducible across machines.

use std::path::Path;

use crate::PathDisplayExt;
use crate::config::{SourceLockEntry, SourcesLockfile};
use crate::errors::{ConfigError, Result};

const LOCKFILE_NAME: &str = "sources.lock";

/// Load `<config_dir>/sources.lock`.
/// Returns an empty lockfile if the file does not exist.
pub fn load_sources_lockfile(config_dir: &Path) -> Result<SourcesLockfile> {
    let path = config_dir.join(LOCKFILE_NAME);
    if !path.exists() {
        return Ok(SourcesLockfile::default());
    }
    let contents = std::fs::read_to_string(&path).map_err(|e| ConfigError::Invalid {
        message: format!("cannot read sources lockfile {}: {e}", path.posix()),
    })?;
    let lockfile: SourcesLockfile = serde_yaml::from_str(&contents).map_err(ConfigError::from)?;
    Ok(lockfile)
}

/// Save `<config_dir>/sources.lock` atomically (temp file + rename).
pub fn save_sources_lockfile(config_dir: &Path, lockfile: &SourcesLockfile) -> Result<()> {
    let path = config_dir.join(LOCKFILE_NAME);
    let contents = serde_yaml::to_string(lockfile).map_err(ConfigError::from)?;
    crate::atomic_write_str(&path, &contents).map_err(|e| ConfigError::Invalid {
        message: format!("cannot write sources lockfile {}: {e}", path.posix()),
    })?;
    Ok(())
}

/// Upsert a single entry (matched by name) into the lockfile, then save.
pub fn update_source_lock_entry(config_dir: &Path, entry: SourceLockEntry) -> Result<()> {
    let mut lockfile = load_sources_lockfile(config_dir)?;
    if let Some(existing) = lockfile.sources.iter_mut().find(|e| e.name == entry.name) {
        *existing = entry;
    } else {
        lockfile.sources.push(entry);
    }
    save_sources_lockfile(config_dir, &lockfile)
}

/// Remove an entry from the lockfile by name, then save (if the entry existed).
/// Called on `source remove` to keep the lockfile in sync with config.
pub fn remove_source_lock_entry(config_dir: &Path, name: &str) -> Result<()> {
    let mut lockfile = load_sources_lockfile(config_dir)?;
    let before = lockfile.sources.len();
    lockfile.sources.retain(|e| e.name != name);
    if lockfile.sources.len() < before {
        save_sources_lockfile(config_dir, &lockfile)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_entry(name: &str, commit: &str) -> SourceLockEntry {
        SourceLockEntry {
            name: name.to_string(),
            url: "https://github.com/org/repo.git".to_string(),
            pin_version: Some("~2".to_string()),
            resolved_ref: Some("v2.1.0".to_string()),
            resolved_commit: commit.to_string(),
            locked_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn sources_lockfile_serde_round_trip() {
        let original = SourcesLockfile {
            sources: vec![
                sample_entry("alpha", &"a".repeat(40)),
                sample_entry("beta", &"b".repeat(40)),
            ],
        };
        let yaml = serde_yaml::to_string(&original).expect("serialize");
        let decoded: SourcesLockfile = serde_yaml::from_str(&yaml).expect("deserialize");
        assert_eq!(decoded.sources.len(), 2);
        assert_eq!(decoded.sources[0].name, "alpha");
        assert_eq!(decoded.sources[1].name, "beta");
        assert_eq!(decoded.sources[0].resolved_commit, "a".repeat(40));
        assert_eq!(decoded.sources[0].pin_version.as_deref(), Some("~2"));
        assert_eq!(decoded.sources[0].resolved_ref.as_deref(), Some("v2.1.0"));
    }

    #[test]
    fn load_sources_lockfile_missing_file_returns_default() {
        let dir = TempDir::new().expect("tempdir");
        let lf = load_sources_lockfile(dir.path()).expect("load");
        assert!(lf.sources.is_empty());
    }

    #[test]
    fn update_source_lock_entry_inserts_and_upserts() {
        let dir = TempDir::new().expect("tempdir");

        let entry_a = sample_entry("alpha", &"a".repeat(40));
        let entry_b = sample_entry("beta", &"b".repeat(40));
        update_source_lock_entry(dir.path(), entry_a).expect("insert alpha");
        update_source_lock_entry(dir.path(), entry_b).expect("insert beta");

        let lf = load_sources_lockfile(dir.path()).expect("load after two inserts");
        assert_eq!(lf.sources.len(), 2);

        // Upsert alpha with a new commit SHA
        let updated_alpha = SourceLockEntry {
            name: "alpha".to_string(),
            url: "https://github.com/org/repo.git".to_string(),
            pin_version: Some("~2".to_string()),
            resolved_ref: Some("v2.2.0".to_string()),
            resolved_commit: "c".repeat(40),
            locked_at: "2026-06-01T00:00:00Z".to_string(),
        };
        update_source_lock_entry(dir.path(), updated_alpha).expect("upsert alpha");

        let lf2 = load_sources_lockfile(dir.path()).expect("load after upsert");
        assert_eq!(
            lf2.sources.len(),
            2,
            "upsert must not add a duplicate entry"
        );
        let alpha = lf2
            .sources
            .iter()
            .find(|e| e.name == "alpha")
            .expect("alpha present");
        assert_eq!(alpha.resolved_commit, "c".repeat(40));
        assert_eq!(alpha.resolved_ref.as_deref(), Some("v2.2.0"));
    }

    #[test]
    fn remove_source_lock_entry_removes_existing_and_noop_on_missing() {
        let dir = TempDir::new().expect("tempdir");
        update_source_lock_entry(dir.path(), sample_entry("alpha", &"a".repeat(40)))
            .expect("insert alpha");
        update_source_lock_entry(dir.path(), sample_entry("beta", &"b".repeat(40)))
            .expect("insert beta");

        remove_source_lock_entry(dir.path(), "alpha").expect("remove alpha");
        let lf = load_sources_lockfile(dir.path()).expect("load after remove");
        assert_eq!(lf.sources.len(), 1, "alpha must be gone");
        assert_eq!(lf.sources[0].name, "beta");

        // no-op on non-existent name — must not error or write
        remove_source_lock_entry(dir.path(), "nonexistent").expect("no-op remove");
        let lf2 = load_sources_lockfile(dir.path()).expect("load after no-op");
        assert_eq!(lf2.sources.len(), 1, "count unchanged after no-op");
    }

    #[test]
    fn load_sources_lockfile_errors_when_path_is_a_directory() {
        let dir = TempDir::new().expect("tempdir");
        // A directory named sources.lock makes path.exists() true but
        // read_to_string fail, exercising the read-error message path.
        std::fs::create_dir(dir.path().join(LOCKFILE_NAME)).expect("mkdir lockfile-as-dir");
        let err = load_sources_lockfile(dir.path()).expect_err("read must fail on a directory");
        assert!(
            err.to_string().contains("cannot read sources lockfile"),
            "unexpected error: {err}",
        );
    }

    #[test]
    fn save_sources_lockfile_errors_when_target_is_nonempty_dir() {
        let dir = TempDir::new().expect("tempdir");
        // sources.lock as a non-empty directory: the atomic temp+rename cannot
        // replace it, exercising the write-error message path.
        let lock_dir = dir.path().join(LOCKFILE_NAME);
        std::fs::create_dir(&lock_dir).expect("mkdir lockfile-as-dir");
        std::fs::write(lock_dir.join("occupant"), b"x").expect("occupy dir");
        let err = save_sources_lockfile(dir.path(), &SourcesLockfile::default())
            .expect_err("write must fail when target is a non-empty dir");
        assert!(
            err.to_string().contains("cannot write sources lockfile"),
            "unexpected error: {err}",
        );
    }
}
