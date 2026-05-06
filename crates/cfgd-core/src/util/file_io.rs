use super::constants::MAX_BACKUP_FILE_SIZE;
use super::fs_perms::file_permissions_mode;
use super::hashing::sha256_hex;

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

    // Preserve permissions of existing file if present
    if let Ok(meta) = std::fs::metadata(target) {
        let _ = tmp.as_file().set_permissions(meta.permissions());
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
