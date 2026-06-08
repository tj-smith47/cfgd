use std::sync::atomic::{AtomicU64, Ordering};

use super::constants::MAX_BACKUP_FILE_SIZE;
use super::fs_perms::file_permissions_mode;
use super::hashing::sha256_hex;
use super::paths::PathDisplayExt;

/// Outcome of [`probe_dir_writable`]: a real-access writability check that asks
/// the kernel whether an entry can be created in a directory, rather than
/// inferring from mode bits. Mode bits are a poor proxy: `readonly()` rejects
/// writes root can perform into a 0550 dir (e.g. Fedora's `/root`) and accepts
/// writes a non-owner cannot. Probing honors uid, root-override, ACLs, and
/// read-only mounts.
#[derive(Debug)]
pub enum DirWritable {
    /// An entry could be created and removed — the directory is writable.
    Writable,
    /// Creation failed with a permission / read-only-filesystem error.
    NotWritable,
    /// Creation failed for some other reason (carries the underlying error).
    Io(std::io::Error),
}

/// Probe whether a new entry can be created in `dir` by actually creating and
/// removing a uniquely-named transient file — the same mutation atomic writes
/// perform. This is the single real-access directory-writability probe; both
/// the file-apply path and the state store route through it so the "can I write
/// here?" question is answered identically everywhere.
///
/// The directory must already exist; callers that need it created first should
/// `create_dir_all` before probing.
pub fn probe_dir_writable(dir: &std::path::Path) -> DirWritable {
    static PROBE_SEQ: AtomicU64 = AtomicU64::new(0);
    let probe = dir.join(format!(
        ".cfgd-write-probe-{}-{}",
        std::process::id(),
        PROBE_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            DirWritable::Writable
        }
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem
            ) =>
        {
            DirWritable::NotWritable
        }
        Err(e) => DirWritable::Io(e),
    }
}

/// Captured state of a file for backup purposes.
#[derive(Debug, Clone)]
pub struct FileState {
    pub content: Vec<u8>,
    pub content_hash: String,
    pub permissions: Option<u32>,
    pub is_symlink: bool,
    pub symlink_target: Option<std::path::PathBuf>,
    /// True if the file exceeded MAX_BACKUP_FILE_SIZE and content was not captured.
    pub oversized: bool,
}

/// Atomically write content to a file using temp-file-then-rename.
///
/// The temp file is created in the same directory as `target` to guarantee a
/// same-filesystem rename (atomic on POSIX). Preserves the permissions of an
/// existing target file if one exists. Creates parent directories as needed.
///
/// Returns the SHA256 hex digest of the written content.
pub fn atomic_write(
    target: &std::path::Path,
    content: &[u8],
) -> std::result::Result<String, std::io::Error> {
    use std::io::Write;

    let parent = target.parent().unwrap_or(std::path::Path::new("."));
    std::fs::create_dir_all(parent)?;

    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(content)?;
    tmp.as_file().sync_all()?;

    // Preserve permissions of existing file if present. A perm-set failure
    // here means the new content gets written with default tempfile perms
    // (0600 on most filesystems, but NFS/FUSE can differ) — surface so callers
    // editing security-sensitive files (SSH keys, age keys) see drift.
    if let Ok(meta) = std::fs::metadata(target)
        && let Err(e) = tmp.as_file().set_permissions(meta.permissions())
    {
        tracing::warn!(
            target = %target.posix(),
            error = %e,
            "atomic_write: failed to restore permissions on temp file before rename",
        );
    }

    let hash = sha256_hex(content);

    // persist() does atomic rename on Unix
    tmp.persist(target).map_err(|e| e.error)?;

    Ok(hash)
}

/// Atomically write string content to a file.
pub fn atomic_write_str(
    target: &std::path::Path,
    content: &str,
) -> std::result::Result<String, std::io::Error> {
    atomic_write(target, content.as_bytes())
}

/// Capture a file's content and metadata for backup.
///
/// Uses `symlink_metadata()` — never follows symlinks. For symlinks, captures
/// the link target path but not the content. For regular files >10 MB, sets
/// `oversized: true` and does not capture content.
///
/// Returns `None` if the file does not exist.
pub fn capture_file_state(
    path: &std::path::Path,
) -> std::result::Result<Option<FileState>, std::io::Error> {
    let symlink_meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };

    if symlink_meta.file_type().is_symlink() {
        let symlink_target = std::fs::read_link(path)?;
        return Ok(Some(FileState {
            content: Vec::new(),
            content_hash: String::new(),
            permissions: None,
            is_symlink: true,
            symlink_target: Some(symlink_target),
            oversized: false,
        }));
    }

    let permissions = file_permissions_mode(&symlink_meta);

    if symlink_meta.len() > MAX_BACKUP_FILE_SIZE {
        return Ok(Some(FileState {
            content: Vec::new(),
            content_hash: String::new(),
            permissions,
            is_symlink: false,
            symlink_target: None,
            oversized: true,
        }));
    }

    let content = std::fs::read(path)?;
    let hash = sha256_hex(&content);

    Ok(Some(FileState {
        content,
        content_hash: hash,
        permissions,
        is_symlink: false,
        symlink_target: None,
        oversized: false,
    }))
}

/// Like `capture_file_state`, but follows symlinks to capture the resolved
/// content. For symlinks, `is_symlink` and `symlink_target` are recorded AND
/// the actual file content behind the symlink is read. This is used for
/// post-apply snapshots where we need to know both the link target and the
/// content that was accessible through the symlink at the time of capture.
///
/// Returns `None` if the file does not exist (or the symlink is dangling).
pub fn capture_file_resolved_state(
    path: &std::path::Path,
) -> std::result::Result<Option<FileState>, std::io::Error> {
    let symlink_meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };

    let is_symlink = symlink_meta.file_type().is_symlink();
    let symlink_target = if is_symlink {
        std::fs::read_link(path).ok()
    } else {
        None
    };

    // Read the actual content (following symlinks)
    let real_meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Dangling symlink
            return Ok(None);
        }
        Err(e) => return Err(e),
    };

    let permissions = file_permissions_mode(&real_meta);

    if real_meta.len() > MAX_BACKUP_FILE_SIZE {
        return Ok(Some(FileState {
            content: Vec::new(),
            content_hash: String::new(),
            permissions,
            is_symlink,
            symlink_target,
            oversized: true,
        }));
    }

    let content = std::fs::read(path)?;
    let hash = sha256_hex(&content);

    Ok(Some(FileState {
        content,
        content_hash: hash,
        permissions,
        is_symlink,
        symlink_target,
        oversized: false,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs as unix_fs;

    #[test]
    fn atomic_write_creates_file_and_returns_hash() {
        let tmp = tempfile::TempDir::new().unwrap();
        let target = tmp.path().join("out.txt");
        let hash = atomic_write(&target, b"hello").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "hello");
        assert!(!hash.is_empty());
        assert_eq!(hash.len(), 64);
    }

    #[test]
    fn atomic_write_creates_parent_dirs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let target = tmp.path().join("a/b/c/file.txt");
        atomic_write(&target, b"nested").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "nested");
    }

    #[test]
    fn atomic_write_str_works() {
        let tmp = tempfile::TempDir::new().unwrap();
        let target = tmp.path().join("str.txt");
        let hash = atomic_write_str(&target, "string content").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "string content");
        assert_eq!(hash.len(), 64);
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_preserves_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let target = tmp.path().join("perms.txt");
        fs::write(&target, "old").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        atomic_write(&target, b"new").unwrap();
        let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
    }

    #[test]
    fn capture_file_state_regular_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("file.txt");
        fs::write(&path, "test content").unwrap();
        let state = capture_file_state(&path).unwrap().unwrap();
        assert_eq!(state.content, b"test content");
        assert!(!state.content_hash.is_empty());
        assert!(!state.is_symlink);
        assert!(state.symlink_target.is_none());
        assert!(!state.oversized);
    }

    #[test]
    fn capture_file_state_nonexistent_returns_none() {
        let path = std::path::Path::new("/no/such/file/abc123");
        assert!(capture_file_state(path).unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn capture_file_state_symlink() {
        let tmp = tempfile::TempDir::new().unwrap();
        let target = tmp.path().join("target.txt");
        let link = tmp.path().join("link.txt");
        fs::write(&target, "target content").unwrap();
        unix_fs::symlink(&target, &link).unwrap();
        let state = capture_file_state(&link).unwrap().unwrap();
        assert!(state.is_symlink);
        assert_eq!(state.symlink_target.as_deref(), Some(target.as_path()));
        assert!(state.content.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn capture_file_resolved_state_follows_symlink() {
        let tmp = tempfile::TempDir::new().unwrap();
        let target = tmp.path().join("real.txt");
        let link = tmp.path().join("sym.txt");
        fs::write(&target, "resolved").unwrap();
        unix_fs::symlink(&target, &link).unwrap();
        let state = capture_file_resolved_state(&link).unwrap().unwrap();
        assert!(state.is_symlink);
        assert_eq!(state.symlink_target.as_deref(), Some(target.as_path()));
        assert_eq!(state.content, b"resolved");
        assert!(!state.oversized);
    }

    #[cfg(unix)]
    #[test]
    fn capture_file_resolved_state_dangling_symlink_returns_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        let link = tmp.path().join("dangling.txt");
        unix_fs::symlink("/no/such/target", &link).unwrap();
        assert!(capture_file_resolved_state(&link).unwrap().is_none());
    }

    #[test]
    fn capture_file_resolved_state_nonexistent_returns_none() {
        let path = std::path::Path::new("/no/such/file/xyz");
        assert!(capture_file_resolved_state(path).unwrap().is_none());
    }

    #[test]
    fn probe_dir_writable_reports_writable_for_normal_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(matches!(
            probe_dir_writable(tmp.path()),
            DirWritable::Writable
        ));
    }

    #[cfg(unix)]
    #[test]
    fn probe_dir_writable_reports_not_writable_for_readonly_dir() {
        use std::os::unix::fs::PermissionsExt;

        // Root bypasses mode bits, so a 0o500 dir is still writable to uid 0.
        if crate::is_root() {
            return;
        }
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("ro");
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
        assert!(matches!(probe_dir_writable(&dir), DirWritable::NotWritable));
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    }
}
