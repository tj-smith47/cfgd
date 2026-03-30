use std::path::{Path, PathBuf};

use crate::errors::CsiError;

const LAST_ACCESS_FILE: &str = ".cfgd-last-access";
const COMPLETE_SENTINEL: &str = ".cfgd-complete";

/// Node-level LRU cache for OCI module artifacts.
///
/// Cache layout: `<root>/<module>/<version>/`
/// Each entry directory contains extracted module content plus:
/// - `.cfgd-last-access` — unix timestamp for LRU tracking
/// - `.cfgd-complete` — sentinel indicating successful extraction
///
/// LRU eviction uses the marker file (filesystem atime is unreliable
/// with noatime/relatime mount options).
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
    /// On cache hit, updates access time for LRU tracking.
    /// On cache miss, pulls the OCI artifact to a temp dir and atomically
    /// moves it into place (safe under concurrent access).
    /// After a pull, runs eviction if over capacity.
    pub fn get_or_pull(
        &self,
        module: &str,
        version: &str,
        oci_ref: &str,
    ) -> Result<PathBuf, CsiError> {
        let entry_dir = self.entry_path(module, version)?;

        if entry_dir.is_dir() && is_complete(&entry_dir) {
            touch_atime(&entry_dir);
            return Ok(entry_dir);
        }

        // Cache miss — pull to temp dir, then atomically move into place
        let tmp_name = format!(".tmp-{}-{}-{}", module, version, std::process::id());
        let tmp_dir = self.root.join(&tmp_name);
        std::fs::create_dir_all(&tmp_dir)?;

        let pull_result = cfgd_core::oci::pull_module(oci_ref, &tmp_dir, false);
        if let Err(e) = pull_result {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            return Err(e.into());
        }

        // Mark complete and set access time
        if let Err(e) = cfgd_core::atomic_write_str(&tmp_dir.join(COMPLETE_SENTINEL), "") {
            tracing::warn!("failed to write cache sentinel: {e}");
        }
        touch_atime(&tmp_dir);

        // Ensure parent dir exists for the final path
        if let Some(parent) = entry_dir.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Atomic move — if another thread already placed the entry, discard ours
        if std::fs::rename(&tmp_dir, &entry_dir).is_err() {
            let _ = std::fs::remove_dir_all(&tmp_dir);
        }

        // Best-effort eviction after pull
        if let Err(e) = self.evict_lru() {
            tracing::warn!(error = %e, "cache eviction failed");
        }

        Ok(entry_dir)
    }

    /// Return the cached path if it exists and is complete, without pulling.
    pub fn get(&self, module: &str, version: &str) -> Option<PathBuf> {
        let entry_dir = self.entry_path(module, version).ok()?;
        if entry_dir.is_dir() && is_complete(&entry_dir) {
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
        // Sort by access time ascending (oldest first)
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
                let _ = std::fs::remove_dir(parent);
            }
            freed += size;
            tracing::info!(path = %path.display(), freed_bytes = size, "evicted cache entry");
        }

        Ok(())
    }

    /// Total bytes used by cached entries (excludes marker files).
    pub fn current_size_bytes(&self) -> u64 {
        dir_size_excluding_markers(&self.root)
    }

    fn entry_path(&self, module: &str, version: &str) -> Result<PathBuf, CsiError> {
        cfgd_core::validate_no_traversal(Path::new(module)).map_err(|e| {
            CsiError::InvalidAttribute {
                key: format!("module: {e}"),
            }
        })?;
        cfgd_core::validate_no_traversal(Path::new(version)).map_err(|e| {
            CsiError::InvalidAttribute {
                key: format!("version: {e}"),
            }
        })?;
        Ok(self.root.join(module).join(version))
    }

    /// List all cache entries as (path, access_time_secs) pairs.
    fn list_entries(&self) -> Result<Vec<(PathBuf, u64)>, CsiError> {
        let mut entries = Vec::new();

        let module_dirs = match std::fs::read_dir(&self.root) {
            Ok(rd) => rd,
            Err(e) => {
                tracing::warn!(path = %self.root.display(), error = %e, "cannot read cache root");
                return Ok(entries);
            }
        };

        for module_entry in module_dirs {
            let module_entry = module_entry?;
            let module_path = module_entry.path();
            if !module_path.is_dir() {
                continue;
            }
            // Skip temp dirs
            if module_path
                .file_name()
                .is_some_and(|n| n.to_str().is_some_and(|s| s.starts_with(".tmp-")))
            {
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

/// Write a marker file with the current unix timestamp for LRU tracking.
fn touch_atime(path: &Path) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let _ = cfgd_core::atomic_write_str(&path.join(LAST_ACCESS_FILE), &now.to_string());
}

/// Read the last-access timestamp from the marker file, or 0 if missing.
fn read_atime(path: &Path) -> u64 {
    std::fs::read_to_string(path.join(LAST_ACCESS_FILE))
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

/// Check if an entry has been fully extracted (sentinel present).
fn is_complete(path: &Path) -> bool {
    path.join(COMPLETE_SENTINEL).exists()
}

/// Recursively compute the total size of files, excluding marker files.
fn dir_size_excluding_markers(path: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                total = total.saturating_add(dir_size_excluding_markers(&p));
            } else {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name == LAST_ACCESS_FILE || name == COMPLETE_SENTINEL {
                    continue;
                }
                if let Ok(meta) = p.metadata() {
                    total = total.saturating_add(meta.len());
                }
            }
        }
    }
    total
}

/// Recursively compute total dir size (all files).
fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                total = total.saturating_add(dir_size(&p));
            } else if let Ok(meta) = p.metadata() {
                total = total.saturating_add(meta.len());
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

    fn populate_entry(dir: &Path, module: &str, version: &str, content_size: usize, atime: u64) {
        let entry = dir.join(module).join(version);
        std::fs::create_dir_all(&entry).unwrap();
        std::fs::write(entry.join("data.txt"), "x".repeat(content_size)).unwrap();
        std::fs::write(entry.join(COMPLETE_SENTINEL), "").unwrap();
        std::fs::write(entry.join(LAST_ACCESS_FILE), atime.to_string()).unwrap();
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
        populate_entry(dir.path(), "mymod", "1.0", 100, 5000);

        let result = cache.get("mymod", "1.0");
        assert!(result.is_some());
        assert_eq!(result.unwrap(), dir.path().join("mymod").join("1.0"));
    }

    #[test]
    fn cache_get_returns_none_for_incomplete_entry() {
        let dir = tempfile::tempdir().unwrap();
        let cache = make_cache(dir.path(), 1024 * 1024);

        // Create entry without completion sentinel
        let entry = dir.path().join("partial").join("1.0");
        std::fs::create_dir_all(&entry).unwrap();
        std::fs::write(entry.join("data.txt"), "some data").unwrap();

        assert!(cache.get("partial", "1.0").is_none());
    }

    #[test]
    fn cache_size_tracking_excludes_markers() {
        let dir = tempfile::tempdir().unwrap();
        let cache = make_cache(dir.path(), 1024 * 1024);

        assert_eq!(cache.current_size_bytes(), 0);

        populate_entry(dir.path(), "mod1", "v1", 1000, 5000);

        let size = cache.current_size_bytes();
        // Should be ~1000 (content) but NOT include marker file sizes
        assert!(size >= 1000);
        assert!(size < 1100); // small tolerance — only data.txt counted
    }

    #[test]
    fn cache_eviction_removes_oldest() {
        let dir = tempfile::tempdir().unwrap();
        let cache = make_cache(dir.path(), 500);

        populate_entry(dir.path(), "old-mod", "v1", 300, 1000);
        populate_entry(dir.path(), "new-mod", "v1", 300, 9999);

        let old_entry = dir.path().join("old-mod").join("v1");
        let new_entry = dir.path().join("new-mod").join("v1");
        assert!(old_entry.exists());
        assert!(new_entry.exists());

        cache.evict_lru().unwrap();

        assert!(!old_entry.exists(), "old entry should have been evicted");
        assert!(new_entry.exists(), "new entry should be retained");
    }

    #[test]
    fn cache_no_eviction_when_under_limit() {
        let dir = tempfile::tempdir().unwrap();
        let cache = make_cache(dir.path(), 1024 * 1024);

        populate_entry(dir.path(), "mod1", "v1", 10, 5000);
        cache.evict_lru().unwrap();
        assert!(dir.path().join("mod1").join("v1").exists());
    }

    #[test]
    fn entry_path_layout() {
        let dir = tempfile::tempdir().unwrap();
        let cache = make_cache(dir.path(), 1024);
        let path = cache.entry_path("nettools", "1.2.3").unwrap();
        assert_eq!(path, dir.path().join("nettools").join("1.2.3"));
    }

    #[test]
    fn entry_path_rejects_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let cache = make_cache(dir.path(), 1024);
        assert!(cache.entry_path("../../etc", "passwd").is_err());
        assert!(cache.entry_path("good-mod", "../../../tmp").is_err());
    }

    #[test]
    fn is_complete_true_when_sentinel_present() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(COMPLETE_SENTINEL), "").unwrap();
        assert!(is_complete(dir.path()));
    }

    #[test]
    fn is_complete_false_when_no_sentinel() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_complete(dir.path()));
    }

    #[test]
    fn cache_eviction_removes_multiple_oldest_until_under_limit() {
        let dir = tempfile::tempdir().unwrap();
        // Each entry has 200 bytes of content; capacity allows only ~1 entry
        let cache = make_cache(dir.path(), 250);

        populate_entry(dir.path(), "mod-a", "v1", 200, 1000);
        populate_entry(dir.path(), "mod-b", "v1", 200, 2000);
        populate_entry(dir.path(), "mod-c", "v1", 200, 3000);
        populate_entry(dir.path(), "mod-d", "v1", 200, 4000);

        // 4 entries x 200 = 800 bytes, capacity is 250
        assert!(cache.current_size_bytes() >= 800);

        cache.evict_lru().unwrap();

        // Oldest entries should be evicted; newest should survive
        assert!(
            !dir.path().join("mod-a").join("v1").exists(),
            "oldest entry should be evicted"
        );
        assert!(
            !dir.path().join("mod-b").join("v1").exists(),
            "second oldest should be evicted"
        );
        assert!(
            !dir.path().join("mod-c").join("v1").exists(),
            "third oldest should be evicted"
        );
        assert!(
            dir.path().join("mod-d").join("v1").exists(),
            "newest entry should survive"
        );

        // After eviction, size should be at or below capacity
        assert!(cache.current_size_bytes() <= 250);
    }

    #[test]
    fn cache_eviction_multiple_versions_of_same_module() {
        let dir = tempfile::tempdir().unwrap();
        let cache = make_cache(dir.path(), 350);

        populate_entry(dir.path(), "nettools", "1.0", 200, 1000);
        populate_entry(dir.path(), "nettools", "2.0", 200, 5000);

        // 400 bytes, capacity 350 — oldest version should be evicted
        cache.evict_lru().unwrap();

        assert!(
            !dir.path().join("nettools").join("1.0").exists(),
            "older version should be evicted"
        );
        assert!(
            dir.path().join("nettools").join("2.0").exists(),
            "newer version should survive"
        );
    }

    #[test]
    fn list_entries_skips_temp_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let cache = make_cache(dir.path(), 1024 * 1024);

        populate_entry(dir.path(), "real-mod", "v1", 100, 5000);

        // Create a temp dir that should be skipped during listing
        let tmp_dir = dir.path().join(".tmp-real-mod-v2-12345");
        std::fs::create_dir_all(&tmp_dir).unwrap();
        std::fs::write(tmp_dir.join("data.txt"), "partial").unwrap();

        let entries = cache.list_entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, dir.path().join("real-mod").join("v1"));
    }

    #[test]
    fn read_atime_returns_zero_for_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_atime(dir.path()), 0);
    }

    #[test]
    fn touch_atime_writes_timestamp() {
        let dir = tempfile::tempdir().unwrap();
        touch_atime(dir.path());

        let atime = read_atime(dir.path());
        // Should be a recent unix timestamp (after 2020)
        assert!(atime > 1_577_836_800);
    }

    #[test]
    fn cache_size_zero_for_empty() {
        let dir = tempfile::tempdir().unwrap();
        let cache = make_cache(dir.path(), 1024);
        assert_eq!(cache.current_size_bytes(), 0);
    }

    #[test]
    fn cache_get_updates_access_time() {
        let dir = tempfile::tempdir().unwrap();
        let cache = make_cache(dir.path(), 1024 * 1024);
        populate_entry(dir.path(), "mymod", "1.0", 100, 1000);

        // Access time should be 1000 initially
        let atime_before = read_atime(&dir.path().join("mymod").join("1.0"));
        assert_eq!(atime_before, 1000);

        // get() should update the access time
        cache.get("mymod", "1.0").unwrap();

        let atime_after = read_atime(&dir.path().join("mymod").join("1.0"));
        assert!(
            atime_after > 1000,
            "access time should be updated after get()"
        );
    }
}
