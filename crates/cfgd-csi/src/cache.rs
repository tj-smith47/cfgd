use std::path::{Path, PathBuf};

use cfgd_core::PathDisplayExt;

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
            if let Err(e) = touch_atime(&entry_dir) {
                tracing::warn!(module = %module, version = %version, error = %e, "failed to update cache atime on hit; LRU ordering may be stale");
            }
            return Ok(entry_dir);
        }

        // Cache miss — pull to temp dir, then atomically move into place
        let tmp_name = format!(".tmp-{}-{}-{}", module, version, std::process::id());
        let tmp_dir = self.root.join(&tmp_name);
        std::fs::create_dir_all(&tmp_dir)?;

        let pull_result = cfgd_core::oci::pull_module(
            oci_ref,
            &tmp_dir,
            cfgd_core::oci::SignaturePolicy::None,
            None,
        );
        if let Err(e) = pull_result {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            return Err(e.into());
        }

        // Mark complete and set access time
        if let Err(e) = cfgd_core::atomic_write_str(&tmp_dir.join(COMPLETE_SENTINEL), "") {
            tracing::warn!("failed to write cache sentinel: {e}");
        }
        if let Err(e) = touch_atime(&tmp_dir) {
            tracing::warn!(module = %module, version = %version, error = %e, "failed to record cache atime after pull; entry will look cold to LRU");
        }

        // Ensure parent dir exists for the final path
        if let Some(parent) = entry_dir.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Atomic move — if another thread already placed the entry, discard ours.
        // On rename failure, we can't blindly `Ok(entry_dir)` — that's only
        // correct when the failure was "lost the race" (another thread/process
        // completed the pull and placed a valid entry). Any other rename error
        // (destination permission issue, parent removed, dest on a different
        // filesystem) leaves `entry_dir` non-existent or incomplete; returning
        // its path would surface later as a confusing "cache entry missing".
        if let Err(e) = std::fs::rename(&tmp_dir, &entry_dir) {
            tracing::warn!(module = %module, version = %version, error = %e, "cache rename race, discarding duplicate pull");
            let _ = std::fs::remove_dir_all(&tmp_dir);
            if !(entry_dir.is_dir() && is_complete(&entry_dir)) {
                return Err(CsiError::Io(std::io::Error::other(format!(
                    "cache rename for {module}:{version} failed and entry is still missing/incomplete after the race: {e}"
                ))));
            }
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
            if let Err(e) = touch_atime(&entry_dir) {
                tracing::warn!(module = %module, version = %version, error = %e, "failed to update cache atime on get; LRU ordering may be stale");
            }
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
                tracing::warn!(path = %path.posix(), error = %e, "failed to evict cache entry");
                continue;
            }
            // Clean up empty parent (module name dir) if no versions remain
            if let Some(parent) = path.parent() {
                let _ = std::fs::remove_dir(parent);
            }
            freed += size;
            tracing::info!(path = %path.posix(), freed_bytes = size, "evicted cache entry");
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
                tracing::warn!(path = %self.root.posix(), error = %e, "cannot read cache root");
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
///
/// Returns the underlying `io::Error` on failure so callers can decide whether
/// to propagate or log. Dropped errors would skew the LRU (stale atime sticks
/// around and makes a hot entry look cold to `evict_lru`).
fn touch_atime(path: &Path) -> std::io::Result<()> {
    let now = cfgd_core::unix_secs_now();
    cfgd_core::atomic_write_str(&path.join(LAST_ACCESS_FILE), &now.to_string())?;
    Ok(())
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
        touch_atime(dir.path()).expect("touch_atime");

        let atime = read_atime(dir.path());
        // Should be a recent unix timestamp (after 2020)
        assert!(atime > 1_577_836_800);
    }

    #[test]
    #[cfg(unix)]
    fn touch_atime_errors_on_unwritable_dir() {
        use std::os::unix::fs::PermissionsExt;
        // atomic_write_str creates parent dirs automatically, so the only
        // reliable failure mode is a parent that exists but is read-only.
        // Root bypasses permission bits on Unix, so skip under euid==0
        // where the write would succeed anyway.
        if cfgd_core::is_root() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let ro = dir.path().join("readonly");
        std::fs::create_dir(&ro).unwrap();
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o500)).unwrap();

        let err = touch_atime(&ro).expect_err("should fail on read-only dir");
        let _ = err.kind();

        // Restore perms so tempdir can clean up.
        let _ = std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o700));
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

    #[test]
    fn cache_get_or_pull_returns_path_without_touching_oci_on_hit() {
        // get_or_pull cache-hit early-return (lines 42-47): pre-populate an
        // entry with the .cfgd-complete sentinel — get_or_pull must short-
        // circuit and return entry_path WITHOUT invoking oci::pull_module,
        // proven by passing a garbage oci_ref that would fail any real call.
        let dir = tempfile::tempdir().unwrap();
        let cache = make_cache(dir.path(), 1024 * 1024);
        populate_entry(dir.path(), "preinstalled", "1.0.0", 256, 1_000);

        let result = cache
            .get_or_pull("preinstalled", "1.0.0", "not-a-real-oci-ref://garbage")
            .expect("cache-hit must NOT consult oci::pull_module");

        assert_eq!(result, dir.path().join("preinstalled").join("1.0.0"));
        assert!(
            result.join(COMPLETE_SENTINEL).exists(),
            "sentinel should still mark the entry complete",
        );
        // The hit path calls touch_atime — verify atime moved forward.
        let new_atime = read_atime(&result);
        assert!(
            new_atime > 1_000,
            "atime must refresh on cache hit (was 1000, now {})",
            new_atime,
        );
    }

    #[test]
    fn list_entries_skips_regular_files_at_root() {
        // list_entries non-dir skip at root level (line 180): a stray regular
        // file directly under the cache root (e.g. a README placed by an
        // operator) must NOT be treated as a module directory. Pin the
        // contract; otherwise eviction would try to remove_dir_all a regular
        // file and the cache would surface bogus errors.
        let dir = tempfile::tempdir().unwrap();
        let cache = make_cache(dir.path(), 1024 * 1024);
        populate_entry(dir.path(), "real-mod", "v1", 100, 5_000);
        std::fs::write(dir.path().join("README"), "not a module dir").unwrap();

        let entries = cache.list_entries().unwrap();
        assert_eq!(entries.len(), 1, "stray file at root must not be listed");
        assert_eq!(entries[0].0, dir.path().join("real-mod").join("v1"));
    }

    #[test]
    fn list_entries_skips_regular_files_at_version_level() {
        // list_entries non-dir skip at module/version level (line 199): a
        // regular file sibling to version dirs inside a module dir (e.g.
        // module-level metadata.json placed by a future feature) must not be
        // listed as a version. Without this skip the per-version atime read
        // would target a non-directory and surface noise.
        let dir = tempfile::tempdir().unwrap();
        let cache = make_cache(dir.path(), 1024 * 1024);
        populate_entry(dir.path(), "vmod", "1.0", 100, 5_000);
        // Sibling of version dir, but not a version itself.
        std::fs::write(dir.path().join("vmod").join("notes.txt"), "stray").unwrap();

        let entries = cache.list_entries().unwrap();
        assert_eq!(
            entries.len(),
            1,
            "stray file under module dir must be skipped"
        );
        assert_eq!(entries[0].0, dir.path().join("vmod").join("1.0"));
    }
}
