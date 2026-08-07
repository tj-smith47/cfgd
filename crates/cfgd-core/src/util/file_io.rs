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

/// Resolve the file a write to `path` must land on.
///
/// A rename replaces a symlink with a regular file, so writing to a symlinked
/// dotfile (the shape stow, chezmoi, and hand-rolled dotfile repos produce)
/// would sever the link: the repo stops receiving the change and the next
/// re-link restores stale content over it. Resolving first writes *through* the
/// link, which is what every editor and shell redirect does.
///
/// Following a link means the *link* chooses the destination, so resolution is
/// allowed only inside the tree the link's own owner already controls: the
/// resolved file must belong to the same uid as the link. Without that bound, a
/// link planted at `~/.cfgd.env` by an unprivileged user redirects the next
/// `sudo -E cfgd apply` — which keeps that user's `HOME` — onto any path on the
/// filesystem, with content the same user authors in their own config. A
/// mismatch is refused rather than downgraded to replacing the link, because
/// silently replacing it is the dotfile-repo destruction that following the
/// link exists to prevent.
///
/// A dangling link keeps the original path — a link to a target that does not
/// exist names no file to write through, and creating one wherever it points
/// would let a broken link choose the destination. Every other resolution
/// failure (`EACCES` on an intermediate directory, `ELOOP`) is propagated: those
/// are not "no link target", and degrading them to replacing the link destroys
/// the link on exactly the paths cfgd understands least.
fn resolve_write_target(
    path: &std::path::Path,
) -> std::result::Result<std::path::PathBuf, std::io::Error> {
    let link_meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(path.to_path_buf()),
        Err(e) => return Err(e),
    };
    if !link_meta.file_type().is_symlink() {
        return Ok(path.to_path_buf());
    }
    let resolved = match std::fs::canonicalize(path) {
        Ok(resolved) => strip_verbatim(resolved),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(path.to_path_buf()),
        Err(e) => return Err(e),
    };
    guard_resolved_owner(path, &link_meta, &resolved)?;
    Ok(resolved)
}

/// Refuse a resolution that crosses an ownership boundary.
///
/// The uid is read back through an `O_NOFOLLOW` open of the resolved path, not
/// a second `stat`: canonicalization and the check are separate syscalls, and
/// `O_NOFOLLOW` makes a final component swapped to a symlink in that window an
/// `ELOOP` error instead of a silent hop to a third file. The rename that
/// follows never re-traverses a link either — `rename(2)` operates on the final
/// component itself — so the path checked here is the path written.
#[cfg(unix)]
fn guard_resolved_owner(
    link: &std::path::Path,
    link_meta: &std::fs::Metadata,
    resolved: &std::path::Path,
) -> std::result::Result<(), std::io::Error> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    let link_uid = link_meta.uid();
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(resolved)?;
    let resolved_uid = file.metadata()?.uid();
    if resolved_uid == link_uid {
        return Ok(());
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        format!(
            "{} is a symlink to {}, which is owned by uid {resolved_uid} while the link itself is \
             owned by uid {link_uid}; refusing to write through it, because a link may only \
             redirect a write inside the tree its own owner already controls. Point the link at a \
             file you own, or replace it with a regular file.",
            link.posix(),
            resolved.posix()
        ),
    ))
}

/// Windows has no uid to compare, and no `sudo -E` equivalent that hands an
/// elevated process an unprivileged user's `HOME`: an elevated PowerShell reads
/// the elevated profile. Creating a symlink there also already requires
/// Developer Mode or elevation, so a link an unprivileged user could plant is
/// not part of the model this bound defends against.
#[cfg(not(unix))]
fn guard_resolved_owner(
    _link: &std::path::Path,
    _link_meta: &std::fs::Metadata,
    _resolved: &std::path::Path,
) -> std::result::Result<(), std::io::Error> {
    Ok(())
}

#[cfg(windows)]
fn strip_verbatim(path: std::path::PathBuf) -> std::path::PathBuf {
    match path.to_str() {
        Some(s) => std::path::PathBuf::from(super::paths::strip_windows_verbatim(s)),
        None => path,
    }
}

#[cfg(not(windows))]
fn strip_verbatim(path: std::path::PathBuf) -> std::path::PathBuf {
    path
}

/// Give `tmp` the ownership the file at `target` must end up with, when the
/// process is elevated and the destination belongs to someone else.
///
/// `sudo -E cfgd apply` keeps the invoking user's `HOME`, so an elevated run
/// writes that user's dotfiles. The temp file is created by root, and a rename
/// carries root ownership onto the destination — after which the user's own
/// unelevated run can no longer read or rewrite their own file. The ownership
/// to reproduce comes from the existing destination, or from its directory when
/// the write creates the file.
///
/// Read with `symlink_metadata`: when `target` is a link this write is about to
/// replace, the owner to reproduce is the link's, not that of whatever it
/// pointed at. The read is a separate syscall from the rename that consumes it,
/// so a target replaced in that window is chowned to a stale owner — acceptable
/// because anyone who can replace the target in that window already owns the
/// path and can chown the result themselves.
#[cfg(unix)]
fn preserve_target_ownership(
    tmp: &tempfile::NamedTempFile,
    target: &std::path::Path,
) -> std::result::Result<(), std::io::Error> {
    use std::os::unix::fs::MetadataExt;

    if !super::process::is_root() {
        return Ok(());
    }
    let owner = std::fs::symlink_metadata(target)
        .ok()
        .or_else(|| {
            target
                .parent()
                .and_then(|dir| std::fs::symlink_metadata(dir).ok())
        })
        .map(|meta| (meta.uid(), meta.gid()));
    let Some((uid, gid)) = owner else {
        return Ok(());
    };
    if uid == 0 && gid == 0 {
        return Ok(());
    }
    std::os::unix::fs::chown(tmp.path(), Some(uid), Some(gid)).map_err(|e| {
        std::io::Error::new(
            e.kind(),
            format!(
                "cannot give {} back to uid {uid}:gid {gid} after an elevated write ({e}); \
                 refusing to leave it owned by root — re-run without sudo, or chown it afterwards",
                target.posix()
            ),
        )
    })
}

#[cfg(not(unix))]
fn preserve_target_ownership(
    _tmp: &tempfile::NamedTempFile,
    _target: &std::path::Path,
) -> std::result::Result<(), std::io::Error> {
    Ok(())
}

/// Atomically write content to a file using temp-file-then-rename.
///
/// The temp file is created in the same directory as `target` to guarantee a
/// same-filesystem rename (atomic on POSIX). Preserves the permissions — and,
/// under an elevated run, the ownership — of an existing target file if one
/// exists. Creates parent directories as needed.
///
/// The rename replaces whatever occupies `target`, a symlink included. To
/// update a user-owned dotfile the user may have symlinked into a dotfile repo,
/// use [`atomic_write_resolved`] instead.
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

    // Ahead of the rename, so a failure to reproduce the ownership leaves the
    // destination untouched instead of root-owned.
    preserve_target_ownership(&tmp, target)?;

    let hash = sha256_hex(content);

    // persist() does atomic rename on Unix
    tmp.persist(target).map_err(|e| e.error)?;

    Ok(hash)
}

/// Create the parent directory of `target` (and every missing ancestor).
///
/// A no-op when `target` has no parent or the directory already exists. Use
/// before writing a file whose directory may not exist yet; to create a named
/// directory itself, call `create_dir_all` directly.
pub fn ensure_parent_dir(target: &std::path::Path) -> std::result::Result<(), std::io::Error> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

/// Atomically write string content to a file.
pub fn atomic_write_str(
    target: &std::path::Path,
    content: &str,
) -> std::result::Result<String, std::io::Error> {
    atomic_write(target, content.as_bytes())
}

/// Atomically write merged content back over a file cfgd does not own,
/// following `target` to the real file when it is a symlink.
///
/// The write publishes the merge where readers of the file see it while
/// leaving the path's identity alone. [`atomic_write`] replaces the path it is
/// handed, so a symlinked target would become a regular file and orphan the
/// file it pointed at — the `~/.gitconfig -> ~/dotfiles/gitconfig` layout is
/// exactly the shape partial-file edits exist to serve, and the merge already
/// read *through* the link. A dangling link resolves to nothing and is written
/// at the link path itself, matching the merge's treatment of a missing target
/// as empty content.
///
/// Returns the SHA256 hex digest of the written content.
pub fn atomic_write_merged(
    target: &std::path::Path,
    content: &str,
) -> std::result::Result<String, std::io::Error> {
    let resolved = if target.is_symlink() {
        std::fs::canonicalize(target).unwrap_or_else(|_| target.to_path_buf())
    } else {
        target.to_path_buf()
    };
    atomic_write_str(&resolved, content)
}

/// Atomically write content to the file `target` ultimately names, following a
/// symlink rather than replacing it.
///
/// The variant for a file the USER owns and may have symlinked into a dotfile
/// repo — shell rc files, the generated env file. [`atomic_write`] would leave
/// a regular file where their link was, so the repo stops receiving the change
/// and the next re-link overwrites it with stale content.
///
/// Deploying a managed file keeps [`atomic_write`]: there, replacing a link
/// with a regular file is the whole point of switching a file from the symlink
/// strategy to the copy strategy.
///
/// Errors with `PermissionDenied` when the link resolves to a file owned by
/// someone other than the link's owner — see [`resolve_write_target`].
pub fn atomic_write_resolved(
    target: &std::path::Path,
    content: &[u8],
) -> std::result::Result<String, std::io::Error> {
    atomic_write(&resolve_write_target(target)?, content)
}

/// Atomically write string content through a symlinked target.
pub fn atomic_write_resolved_str(
    target: &std::path::Path,
    content: &str,
) -> std::result::Result<String, std::io::Error> {
    atomic_write_resolved(target, content.as_bytes())
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
/// post-apply snapshots that need both the link target and the
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

        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("ro");
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();

        let probed = probe_dir_writable(&dir);
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();

        // The probe answers "can THIS process write here", not "are the mode
        // bits set": uid 0 bypasses them, so root must be told it can write.
        // Both arms assert, so the root runner is not a silent no-op.
        if crate::is_root() {
            assert!(matches!(probed, DirWritable::Writable));
        } else {
            assert!(matches!(probed, DirWritable::NotWritable));
        }
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_replaces_a_symlink_with_a_regular_file() {
        // Switching a managed file from the symlink strategy to the copy
        // strategy has to leave a real file where the link was, and must not
        // write through it onto the module's own source.
        let tmp = tempfile::TempDir::new().unwrap();
        let real = tmp.path().join("source");
        fs::write(&real, "source\n").unwrap();
        let link = tmp.path().join("deployed");
        unix_fs::symlink(&real, &link).unwrap();

        atomic_write(&link, b"deployed\n").unwrap();

        assert!(
            !fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&link).unwrap(), "deployed\n");
        assert_eq!(fs::read_to_string(&real).unwrap(), "source\n");
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_resolved_through_a_symlink_keeps_the_link() {
        // Dotfile managers (stow, chezmoi) leave ~/.bashrc as a symlink into a
        // git repo. Renaming over it would break the link and orphan the repo
        // copy, so the write must land on the file the link points at.
        let tmp = tempfile::TempDir::new().unwrap();
        let real = tmp.path().join("repo/bashrc");
        fs::create_dir_all(real.parent().unwrap()).unwrap();
        fs::write(&real, "original\n").unwrap();
        let link = tmp.path().join(".bashrc");
        unix_fs::symlink("repo/bashrc", &link).unwrap();

        atomic_write_resolved(&link, b"updated\n").unwrap();

        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the symlink itself must survive the write"
        );
        assert_eq!(fs::read_to_string(&real).unwrap(), "updated\n");
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_resolved_to_a_dangling_symlink_writes_at_the_link_path() {
        // A link with no target names no file to write through. Creating one at
        // wherever it points would let a planted link choose the destination, so
        // the write stays at the path the caller asked for.
        let tmp = tempfile::TempDir::new().unwrap();
        let link = tmp.path().join(".bashrc");
        let nowhere = tmp.path().join("nowhere");
        unix_fs::symlink(&nowhere, &link).unwrap();

        atomic_write_resolved(&link, b"content\n").unwrap();

        assert_eq!(fs::read_to_string(&link).unwrap(), "content\n");
        assert!(
            !nowhere.exists(),
            "a dangling link must not decide where the file lands"
        );
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_preserves_a_non_root_owner_when_elevated() {
        use std::os::unix::fs::MetadataExt;

        // Both arms assert. Only uid 0 can reassign ownership, so an
        // unprivileged runner cannot stage a foreign-owned target — but it can
        // still pin the other half of the contract (an unelevated write leaves
        // the owner exactly as it found it), which keeps this test from
        // reporting green while verifying nothing.
        let tmp = tempfile::TempDir::new().unwrap();
        let target = tmp.path().join("dotfile");
        fs::write(&target, "original\n").unwrap();
        let before = fs::metadata(&target).unwrap();
        let expected = if crate::is_root() {
            std::os::unix::fs::chown(&target, Some(12345), Some(12345)).unwrap();
            (12345, 12345)
        } else {
            (before.uid(), before.gid())
        };

        atomic_write(&target, b"updated\n").unwrap();

        let meta = fs::metadata(&target).unwrap();
        assert_eq!(
            (meta.uid(), meta.gid()),
            expected,
            "a write must never change who owns the file it replaces"
        );
        assert_eq!(fs::read_to_string(&target).unwrap(), "updated\n");
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_creating_a_file_inherits_the_directory_owner_when_elevated() {
        use std::os::unix::fs::MetadataExt;

        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        fs::create_dir(&home).unwrap();
        let expected = if crate::is_root() {
            std::os::unix::fs::chown(&home, Some(12345), Some(12345)).unwrap();
            (12345, 12345)
        } else {
            let dir = fs::metadata(&home).unwrap();
            (dir.uid(), dir.gid())
        };
        let target = home.join(".cfgd.env");

        atomic_write(&target, b"export FOO=\"bar\"\n").unwrap();

        let meta = fs::metadata(&target).unwrap();
        assert_eq!(
            (meta.uid(), meta.gid()),
            expected,
            "a created file must belong to the owner of the directory holding it"
        );
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_resolved_refuses_a_link_pointing_at_another_uids_file() {
        // The elevated-redirect escape: an unprivileged user plants a link in
        // their own home, and the next `sudo -E cfgd apply` would follow it onto
        // a file they do not own. Only uid 0 can stage a foreign-owned target,
        // so the unprivileged arm asserts the other half of the contract — a
        // link inside its own owner's tree is followed, not refused — rather
        // than returning green having verified nothing.
        let tmp = tempfile::TempDir::new().unwrap();
        let outside = tmp.path().join("outside");
        fs::write(&outside, "untouched\n").unwrap();
        let link = tmp.path().join(".cfgd.env");
        unix_fs::symlink(&outside, &link).unwrap();
        if crate::is_root() {
            std::os::unix::fs::chown(&link, Some(12345), Some(12345)).unwrap();
        } else {
            std::os::unix::fs::chown(&outside, Some(0), Some(0))
                .expect_err("an unprivileged process must not be able to stage this");
            // Unprivileged, the link and its target share this uid, so the
            // refusal cannot be staged — assert the permitted case instead.
            atomic_write_resolved(&link, b"written\n").unwrap();
            assert_eq!(fs::read_to_string(&outside).unwrap(), "written\n");
            return;
        }

        let err = atomic_write_resolved(&link, b"written\n")
            .expect_err("a link to another uid's file must be refused");

        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(
            fs::read_to_string(&outside).unwrap(),
            "untouched\n",
            "the file the link pointed at must be byte-identical"
        );
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "refusing must not degrade to replacing the link"
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_write_target_propagates_a_resolution_error_that_is_not_a_dangling_link() {
        let tmp = tempfile::TempDir::new().unwrap();
        let link = tmp.path().join("loop-a");
        let other = tmp.path().join("loop-b");
        unix_fs::symlink(&other, &link).unwrap();
        unix_fs::symlink(&link, &other).unwrap();

        let err = resolve_write_target(&link).expect_err("a symlink loop must not be swallowed");

        assert_ne!(
            err.kind(),
            std::io::ErrorKind::NotFound,
            "only a dangling link may degrade to the caller's own path"
        );
    }
}
