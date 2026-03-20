use std::path::{Path, PathBuf};

use crate::errors::CsiError;

/// Node-level LRU cache for OCI module artifacts.
///
/// Cache layout: `<root>/<module>/<version>/`
/// Each entry directory contains the extracted module content.
/// LRU eviction uses a `.cfgd-last-access` marker file with unix timestamp.
pub struct Cache {
    root: PathBuf,
    max_bytes: u64,
}

impl Cache {
    pub fn new(root: PathBuf, max_bytes: u64) -> Result<Self, CsiError> {
        std::fs::create_dir_all(&root)?;
        Ok(Self { root, max_bytes })
    }

    /// Return the cache path for a module, or pull it if not cached.
    ///
    /// On cache hit, updates atime for LRU tracking.
    /// On cache miss, pulls the OCI artifact and extracts to cache.
    /// After a pull, runs eviction if over capacity.
    pub fn get_or_pull(
        &self,
        module: &str,
        version: &str,
        oci_ref: &str,
    ) -> Result<PathBuf, CsiError> {
        let entry_dir = self.entry_path(module, version);

        if entry_dir.is_dir() && has_content(&entry_dir) {
            touch_atime(&entry_dir);
            return Ok(entry_dir);
        }

        // Cache miss — pull from OCI registry
        std::fs::create_dir_all(&entry_dir)?;
        cfgd_core::oci::pull_module(oci_ref, &entry_dir, false)?;

        touch_atime(&entry_dir);

        // Best-effort eviction after pull
        if let Err(e) = self.evict_lru() {
            tracing::warn!(error = %e, "cache eviction failed");
        }

        Ok(entry_dir)
    }

    /// Return the cached path if it exists, without pulling.
    pub fn get(&self, module: &str, version: &str) -> Option<PathBuf> {
        let entry_dir = self.entry_path(module, version);
        if entry_dir.is_dir() && has_content(&entry_dir) {
            touch_atime(&entry_dir);
            Some(entry_dir)
        } else {
            None
        }
    }

    /// Evict least-recently-used entries until cache is under max_bytes.
    pub fn evict_lru(&self) -> Result<(), CsiError> {
        let current = self.current_size_bytes();
        if current <= self.max_bytes {
            return Ok(());
        }

        let mut entries = self.list_entries()?;
        // Sort by atime ascending (oldest first)
        entries.sort_by_key(|(_, atime)| *atime);

        let mut freed = 0u64;
        let overflow = current.saturating_sub(self.max_bytes);

        for (path, _) in &entries {
            if freed >= overflow {
                break;
            }
            let size = dir_size(path);
            if let Err(e) = std::fs::remove_dir_all(path) {
                tracing::warn!(path = %path.display(), error = %e, "failed to evict cache entry");
                continue;
            }
            // Clean up empty parent (module name dir) if no versions remain
            if let Some(parent) = path.parent() {
                let _ = std::fs::remove_dir(parent); // fails silently if not empty
            }
            freed += size;
            tracing::info!(path = %path.display(), freed_bytes = size, "evicted cache entry");
        }

        Ok(())
    }

    /// Total bytes used by cached entries.
    pub fn current_size_bytes(&self) -> u64 {
        dir_size(&self.root)
    }

    fn entry_path(&self, module: &str, version: &str) -> PathBuf {
        self.root.join(module).join(version)
    }

    /// List all cache entries as (path, atime_secs) pairs.
    fn list_entries(&self) -> Result<Vec<(PathBuf, i64)>, CsiError> {
        let mut entries = Vec::new();

        let module_dirs = match std::fs::read_dir(&self.root) {
            Ok(rd) => rd,
            Err(_) => return Ok(entries),
        };

        for module_entry in module_dirs {
            let module_entry = module_entry?;
            let module_path = module_entry.path();
            if !module_path.is_dir() {
                continue;
            }

            let version_dirs = match std::fs::read_dir(&module_path) {
                Ok(rd) => rd,
                Err(_) => continue,
            };

            for version_entry in version_dirs {
                let version_entry = version_entry?;
                let version_path = version_entry.path();
                if !version_path.is_dir() {
                    continue;
                }

                let atime = read_atime(&version_path);

                entries.push((version_path, atime));
            }
        }

        Ok(entries)
    }
}

const LAST_ACCESS_FILE: &str = ".cfgd-last-access";

/// Write a marker file with the current unix timestamp for LRU tracking.
/// More reliable than filesystem atime which may be disabled (noatime/relatime).
fn touch_atime(path: &Path) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let _ = std::fs::write(path.join(LAST_ACCESS_FILE), now.to_string());
}

/// Read the last-access timestamp from the marker file, or 0 if missing.
fn read_atime(path: &Path) -> i64 {
    std::fs::read_to_string(path.join(LAST_ACCESS_FILE))
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(0)
}

/// Check if a directory has at least one child entry.
fn has_content(path: &Path) -> bool {
    std::fs::read_dir(path)
        .map(|mut rd| rd.next().is_some())
        .unwrap_or(false)
}

/// Recursively compute the total size of files in a directory.
fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                total += dir_size(&p);
            } else if let Ok(meta) = p.metadata() {
                total += meta.len();
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_cache(dir: &Path, max_bytes: u64) -> Cache {
        Cache::new(dir.to_path_buf(), max_bytes).unwrap()
    }

    #[test]
    fn cache_get_returns_none_when_empty() {
        let dir = tempfile::tempdir().unwrap();
        let cache = make_cache(dir.path(), 1024 * 1024);
        assert!(cache.get("nettools", "1.0").is_none());
    }

    #[test]
    fn cache_get_returns_path_after_manual_populate() {
        let dir = tempfile::tempdir().unwrap();
        let cache = make_cache(dir.path(), 1024 * 1024);

        // Manually create a cache entry
        let entry = dir.path().join("mymod").join("1.0");
        std::fs::create_dir_all(&entry).unwrap();
        std::fs::write(entry.join("module.yaml"), "name: mymod\n").unwrap();

        let result = cache.get("mymod", "1.0");
        assert!(result.is_some());
        assert_eq!(result.unwrap(), entry);
    }

    #[test]
    fn cache_size_tracking() {
        let dir = tempfile::tempdir().unwrap();
        let cache = make_cache(dir.path(), 1024 * 1024);

        assert_eq!(cache.current_size_bytes(), 0);

        // Add some content
        let entry = dir.path().join("mod1").join("v1");
        std::fs::create_dir_all(&entry).unwrap();
        std::fs::write(entry.join("data.txt"), "x".repeat(1000)).unwrap();

        assert!(cache.current_size_bytes() >= 1000);
    }

    #[test]
    fn cache_eviction_removes_oldest() {
        let dir = tempfile::tempdir().unwrap();
        // Max 500 bytes — both entries together exceed this
        let cache = make_cache(dir.path(), 500);

        // Create two entries with different access times
        let old_entry = dir.path().join("old-mod").join("v1");
        std::fs::create_dir_all(&old_entry).unwrap();
        std::fs::write(old_entry.join("data.txt"), "x".repeat(300)).unwrap();
        std::fs::write(old_entry.join(LAST_ACCESS_FILE), "1000").unwrap();

        let new_entry = dir.path().join("new-mod").join("v1");
        std::fs::create_dir_all(&new_entry).unwrap();
        std::fs::write(new_entry.join("data.txt"), "x".repeat(300)).unwrap();
        std::fs::write(new_entry.join(LAST_ACCESS_FILE), "9999").unwrap();

        // Both exist
        assert!(old_entry.exists());
        assert!(new_entry.exists());

        // Evict — should remove old entry
        cache.evict_lru().unwrap();

        assert!(!old_entry.exists(), "old entry should have been evicted");
        assert!(new_entry.exists(), "new entry should be retained");
    }

    #[test]
    fn cache_no_eviction_when_under_limit() {
        let dir = tempfile::tempdir().unwrap();
        let cache = make_cache(dir.path(), 1024 * 1024); // 1MB limit

        let entry = dir.path().join("mod1").join("v1");
        std::fs::create_dir_all(&entry).unwrap();
        std::fs::write(entry.join("data.txt"), "small").unwrap();

        cache.evict_lru().unwrap();
        assert!(entry.exists(), "entry under limit should not be evicted");
    }

    #[test]
    fn entry_path_layout() {
        let dir = tempfile::tempdir().unwrap();
        let cache = make_cache(dir.path(), 1024);
        let path = cache.entry_path("nettools", "1.2.3");
        assert_eq!(path, dir.path().join("nettools").join("1.2.3"));
    }

    #[test]
    fn has_content_true_when_populated() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("file.txt"), "data").unwrap();
        assert!(has_content(dir.path()));
    }

    #[test]
    fn has_content_false_when_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!has_content(dir.path()));
    }
}
