/// Create a symbolic link. On Unix, uses `std::os::unix::fs::symlink`.
/// On Windows, uses `symlink_file` or `symlink_dir` based on what the target
/// resolves to from the link's own parent.
/// If symlink creation fails on Windows due to insufficient privileges,
/// returns an error with guidance to enable Developer Mode or run as admin.
pub fn create_symlink(source: &std::path::Path, target: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        create_symlink_impl(source, target)
    }
    #[cfg(windows)]
    {
        create_symlink_impl(source, target).map_err(|e| symlink_error(source, target, e))
    }
}

/// Windows' `ERROR_PRIVILEGE_NOT_HELD`: the host will not make symbolic links
/// for this user at all.
#[cfg(any(windows, test))]
const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;

/// The ONE wording for a host that refused to create a symbolic link.
///
/// A refusal reaches an operator through whatever verb was copying — a backup
/// sidecar, a restore's staging, an adopted target — so `os error 1314` is the
/// one thing it must never be: the number names neither the link nor the fix.
/// `mklink /J` needs no privilege at all and Rust reports the junction it makes
/// as a symlink, so a stock Windows box can hold links it cannot recreate, and
/// the sentence has to say what to turn on.
///
/// Compiled under `test` on every host: the mapping is pure, and pinning it
/// needs a `raw_os_error`, not a Windows kernel.
#[cfg(any(windows, test))]
pub(crate) fn symlink_error(
    dest: &std::path::Path,
    link: &std::path::Path,
    err: std::io::Error,
) -> std::io::Error {
    use super::paths::PathDisplayExt;
    if err.raw_os_error() != Some(ERROR_PRIVILEGE_NOT_HELD) {
        return err;
    }
    std::io::Error::new(
        err.kind(),
        format!(
            "symlink creation requires Developer Mode or admin privileges: {} -> {}\n\
             Enable Developer Mode: Settings > Update & Security > For developers",
            dest.posix(),
            link.posix()
        ),
    )
}

/// Where to look to decide whether a link points at a directory.
///
/// `dest` is the link's TARGET STRING, written back verbatim by every copy that
/// preserves links, so a relative one is relative to the LINK's own parent.
/// Probing the bare string asks the process CWD instead, and a directory link
/// then lands as `symlink_file` — the wrong reparse type, which Windows will
/// not traverse as a directory.
#[cfg(any(windows, test))]
pub(crate) fn symlink_dir_probe(
    dest: &std::path::Path,
    link: &std::path::Path,
) -> std::path::PathBuf {
    if dest.is_absolute() {
        return dest.to_path_buf();
    }
    link.parent()
        .map_or_else(|| dest.to_path_buf(), |parent| parent.join(dest))
}

#[cfg(unix)]
fn create_symlink_impl(source: &std::path::Path, target: &std::path::Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(source, target)
}

#[cfg(windows)]
fn create_symlink_impl(source: &std::path::Path, target: &std::path::Path) -> std::io::Result<()> {
    if symlink_dir_probe(source, target).is_dir() {
        std::os::windows::fs::symlink_dir(source, target)
    } else {
        std::os::windows::fs::symlink_file(source, target)
    }
}

/// Get Unix permission mode bits from file metadata. Returns None on Windows.
#[cfg(unix)]
pub fn file_permissions_mode(metadata: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(metadata.permissions().mode() & 0o777)
}

#[cfg(windows)]
pub fn file_permissions_mode(_metadata: &std::fs::Metadata) -> Option<u32> {
    None
}

/// Get Unix permission mode bits INCLUDING the setuid/setgid/sticky bits.
/// Returns None on Windows.
///
/// [`file_permissions_mode`] masks to `0o777`, which is what drift comparison
/// against a declared `permissions:` wants for the common case. Two callers need
/// the full `0o7777` instead: a backup sidecar, which must reproduce the file it
/// preserves rather than a de-fanged copy of it, and the module-file convergence
/// check, which compares against a declared mode `parse_octal_mode` already
/// accepts up to `0o7777` — masked to `0o777`, a declared `4755` can never equal
/// the actual mode, so the short-circuit is disabled for the life of the file.
#[cfg(unix)]
pub fn file_permissions_mode_full(metadata: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(metadata.permissions().mode() & 0o7777)
}

#[cfg(windows)]
pub fn file_permissions_mode_full(_metadata: &std::fs::Metadata) -> Option<u32> {
    None
}

/// Parse an octal Unix permission string (e.g. "600", "0755", "0o644") into mode bits.
/// Rejects values above 0o7777 (the valid permission + special-bit range).
pub fn parse_octal_mode(s: &str) -> Result<u32, crate::errors::ConfigError> {
    let trimmed = s.trim().trim_start_matches("0o");
    let mode =
        u32::from_str_radix(trimmed, 8).map_err(|_| crate::errors::ConfigError::Invalid {
            message: format!("invalid octal permission mode '{s}'"),
        })?;
    if mode > 0o7777 {
        return Err(crate::errors::ConfigError::Invalid {
            message: format!("permission mode '{s}' exceeds 0o7777"),
        });
    }
    Ok(mode)
}

/// Set Unix permission mode bits on a file. No-op on Windows (NTFS uses inherited ACLs).
#[cfg(unix)]
pub fn set_file_permissions(path: &std::path::Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(windows)]
pub fn set_file_permissions(_path: &std::path::Path, _mode: u32) -> std::io::Result<()> {
    tracing::debug!("set_file_permissions is a no-op on Windows (NTFS uses inherited ACLs)");
    Ok(())
}

/// Set Unix permission mode bits on a file WITHOUT following a final symlink.
///
/// Reach for this instead of [`set_file_permissions`] wherever an elevated run
/// chmods a path inside a directory an unprivileged user owns: their own home,
/// their `~/.ssh`, the directory a displaced target sat in. `std::fs::set_permissions`
/// resolves the path again and follows whatever link it finds, so between cfgd
/// writing a file and cfgd chmodding it the owner of that directory can unlink it
/// and plant a symlink at another user's private key. The mode then lands on the
/// link's target. Here the path is resolved once, `O_NOFOLLOW` refuses a symlink
/// outright (`ELOOP`), and the mode is set through that descriptor, so a swap
/// after the open cannot move it either.
///
/// A symlink at `path` is an error, never a follow. That is the point: a caller
/// whose declared mode belongs to the file a link points at names THAT file
/// (a `strategy: Symlink` entry names its source, a rollback the destination it
/// recorded) and still chmods it here, rather than letting the chmod resolve a
/// link whose owner may have re-pointed it.
///
/// No-op on Windows, like [`set_file_permissions`].
#[cfg(unix)]
pub fn set_file_permissions_nofollow(path: &std::path::Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let file = open_nofollow(path)?;
    file.set_permissions(std::fs::Permissions::from_mode(mode))
}

#[cfg(windows)]
pub fn set_file_permissions_nofollow(_path: &std::path::Path, _mode: u32) -> std::io::Result<()> {
    tracing::debug!(
        "set_file_permissions_nofollow is a no-op on Windows (NTFS uses inherited ACLs)"
    );
    Ok(())
}

/// OR `bits` onto the mode a file already carries, reading and writing it through
/// ONE [`set_file_permissions_nofollow`]-style descriptor.
///
/// The read and the write must share a descriptor: reading the mode by path and
/// setting it by path resolves twice, which is the race the no-follow open exists
/// to close. The OR is what keeps a mode its owner chose (a `0o664`
/// `/etc/environment`) rather than stamping an absolute one.
///
/// No-op on Windows, where there are no mode bits to widen.
#[cfg(unix)]
pub fn widen_file_permissions_nofollow(path: &std::path::Path, bits: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let file = open_nofollow(path)?;
    let mode = file.metadata()?.permissions().mode();
    file.set_permissions(std::fs::Permissions::from_mode(mode | bits))
}

#[cfg(windows)]
pub fn widen_file_permissions_nofollow(_path: &std::path::Path, _bits: u32) -> std::io::Result<()> {
    tracing::debug!("widen_file_permissions_nofollow is a no-op on Windows");
    Ok(())
}

/// Open `path` for reading, refusing a final symlink.
///
/// Read-only because `fchmod(2)` needs no write access, and a key file cfgd is
/// about to tighten may well be unwritable. The open still needs READ access,
/// which `chmod(2)` did not: a target, or the source a `strategy: Symlink` entry
/// names, left unreadable by its owner (`0o000`, `0o200`, `0o300`) can no longer
/// have its mode set. A linked entry's declared mode belongs to the source file,
/// so the source is the path the chmod names and the refusal reaches it on the
/// same terms as any target.
///
/// That is a narrow, loud and recoverable price for the property. The declared
/// mode is chmodded only where it differs from the file's current one (or on a
/// re-link), so a file already AT its read-less mode is never chmodded again; the
/// case that fails fails with the real `EACCES` on its own action rather than
/// silently, and `chmod +r` on the named file clears it for the next run. An
/// elevated run is unaffected, root bypassing the read check, and that is the run
/// the no-follow open exists for.
///
/// The portable alternatives are worse. Linux's `fchmodat` rejects
/// `AT_SYMLINK_NOFOLLOW` and an `O_PATH` descriptor cannot be `fchmod`ded, so the
/// only ways back are a path-based chmod (the misdirection this primitive exists
/// to refuse) or a Linux-only `/proc/self/fd` hop beside a separate macOS
/// `lchmod` arm: new `libc` surface on two platforms, plus a `/proc` a container
/// need not mount, to serve a mode nobody can read.
#[cfg(unix)]
fn open_nofollow(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

/// Check if a file is executable.
/// Unix: checks the executable bit in mode.
/// Windows: checks file extension against known executable types.
#[cfg(unix)]
pub fn is_executable(_path: &std::path::Path, metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(windows)]
pub fn is_executable(path: &std::path::Path, _metadata: &std::fs::Metadata) -> bool {
    const EXECUTABLE_EXTENSIONS: &[&str] = &["exe", "cmd", "bat", "ps1", "com"];
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| EXECUTABLE_EXTENSIONS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// A file's identity on its filesystem: inode + device on Unix, file index +
/// volume serial on Windows.
///
/// Two paths yielding the same identity name one file — that is
/// [`is_same_inode`], which is built from this. The OTHER question it answers is
/// the one a long-lived handle has to ask: does this path STILL name the file I
/// opened? After a rename there is no second path left to compare, so a holder
/// captures the identity at open and re-derives it from the path later. A daemon
/// holding a SQLite connection while a CLI run migrates the database out from
/// under it would otherwise write into an orphaned inode with no error and no
/// log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIdentity {
    device: u64,
    file: u64,
}

/// The identity of the file `path` names right now, or `None` when the question
/// could not be answered — the file is gone, or the probe itself failed.
///
/// Reach for [`try_file_identity`] where the difference matters: a caller acting
/// on "this is no longer the file that was opened" must not act the same way on
/// "the question could not be asked".
pub fn file_identity(path: &std::path::Path) -> Option<FileIdentity> {
    try_file_identity(path).ok()
}

/// The identity of the file `path` names right now, reporting WHY when it cannot
/// be determined.
///
/// [`std::io::ErrorKind::NotFound`] is the answer "nothing is there", which is
/// the only one that means the path stopped naming what it used to. Every other
/// error (a directory that lost `+x`, a sharing violation from a third party on
/// Windows) means the probe failed, and a caller comparing against a captured
/// identity should hold what it has rather than act on a question it could not
/// ask.
#[cfg(unix)]
pub fn try_file_identity(path: &std::path::Path) -> std::io::Result<FileIdentity> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path)?;
    Ok(FileIdentity {
        device: meta.dev(),
        file: meta.ino(),
    })
}

/// Check if two paths refer to the same file (same inode on Unix, same file index on Windows).
pub fn is_same_inode(a: &std::path::Path, b: &std::path::Path) -> bool {
    match (file_identity(a), file_identity(b)) {
        (Some(ia), Some(ib)) => ia == ib,
        _ => false,
    }
}

/// The Windows arm of [`try_file_identity`]. The probe is a `File::open` rather
/// than a stat, so it can also fail with a sharing violation.
#[cfg(windows)]
pub fn try_file_identity(path: &std::path::Path) -> std::io::Result<FileIdentity> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::BY_HANDLE_FILE_INFORMATION;
    use windows_sys::Win32::Storage::FileSystem::GetFileInformationByHandle;

    let file = std::fs::File::open(path)?;
    // SAFETY: `BY_HANDLE_FILE_INFORMATION` is a plain-old-data struct of
    // integer fields; the all-zero bit pattern is a valid initial value
    // that `GetFileInformationByHandle` overwrites before it is read.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: `file.as_raw_handle()` returns a valid, open Win32 file
    // handle owned by `file`, which outlives the call. `&mut info`
    // points to sufficient, aligned, writable memory for the out
    // parameter. No aliasing: `info` is stack-local.
    let ret = unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut info) };
    if ret == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(FileIdentity {
        device: u64::from(info.dwVolumeSerialNumber),
        file: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    })
}

#[cfg(test)]
mod tests {
    use super::{parse_octal_mode, try_file_identity};

    #[test]
    fn a_missing_path_is_reported_as_not_found() {
        // The one error kind that means "the path stopped naming what it used
        // to". A caller comparing against a captured identity acts on this and
        // on nothing else.
        let tmp = tempfile::tempdir().unwrap();
        let err = try_file_identity(&tmp.path().join("absent")).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    // Unix-only: Windows maps a path through a regular file to
    // ERROR_PATH_NOT_FOUND, the same `ErrorKind::NotFound` a genuinely
    // missing parent directory gives, so the distinction this test pins is
    // not expressible there for this shape.
    #[cfg(unix)]
    #[test]
    fn a_probe_that_could_not_run_is_not_reported_as_not_found() {
        // Walking THROUGH a regular file is a probe failure, not an absence.
        // Reading it as absence is what would close a working connection over a
        // question that was never answered.
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("regular");
        std::fs::write(&file, b"x").unwrap();
        let err = try_file_identity(&file.join("under-a-file")).unwrap_err();
        assert_ne!(err.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn an_existing_file_reports_one_stable_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("regular");
        std::fs::write(&file, b"x").unwrap();
        let first = try_file_identity(&file).unwrap();
        assert_eq!(first, try_file_identity(&file).unwrap());

        // Replacing the file at the same path is a different identity, which is
        // the whole of what a holder is asking about. The replacement is
        // RENAMED over the path, which is what the production case does (a
        // state-directory migration) and what keeps the claim true on every
        // filesystem: unlink-then-create hands the same inode straight back on
        // ext4, so the simulation, not the product, would be what failed.
        let replacement = tmp.path().join("replacement");
        std::fs::write(&replacement, b"y").unwrap();
        std::fs::rename(&replacement, &file).unwrap();
        assert_ne!(first, try_file_identity(&file).unwrap());
    }

    #[test]
    fn parse_octal_mode_plain() {
        assert_eq!(parse_octal_mode("755").unwrap(), 0o755);
        assert_eq!(parse_octal_mode("600").unwrap(), 0o600);
    }

    #[test]
    fn parse_octal_mode_prefix_forms() {
        assert_eq!(parse_octal_mode("0o644").unwrap(), 0o644);
        assert_eq!(parse_octal_mode("0644").unwrap(), 0o644);
    }

    #[test]
    fn parse_octal_mode_trims_whitespace() {
        assert_eq!(parse_octal_mode("  640  ").unwrap(), 0o640);
    }

    #[test]
    fn parse_octal_mode_invalid_radix_errs() {
        assert!(parse_octal_mode("9zz").is_err());
    }

    #[test]
    fn parse_octal_mode_overflow_errs() {
        // "10000" parses as 0o10000 which exceeds 0o7777.
        assert!(parse_octal_mode("10000").is_err());
    }

    /// Both no-follow chmods refuse a symlink, and the file it points at keeps
    /// its mode.
    ///
    /// `O_NOFOLLOW` is the whole of the defence, and it lives in this crate: a
    /// caller's own pin stays green while the flag is gone, because the chmod it
    /// asserts still succeeds on the regular file it planted. What breaks without
    /// the flag is the link case, so the link case is pinned beside the flag.
    #[cfg(unix)]
    #[test]
    fn a_symlink_is_refused_by_both_nofollow_chmods_and_keeps_its_targets_mode() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let secret = tmp.path().join("id_ed25519");
        std::fs::write(&secret, b"private").unwrap();
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = tmp.path().join("declared");
        std::os::unix::fs::symlink(&secret, &link).unwrap();

        let declared = super::set_file_permissions_nofollow(&link, 0o644);
        assert!(
            declared.is_err(),
            "a declared mode must refuse a symlink, got {declared:?}"
        );
        let widened = super::widen_file_permissions_nofollow(&link, 0o044);
        assert!(
            widened.is_err(),
            "a widen must refuse a symlink, got {widened:?}"
        );
        let mode = std::fs::metadata(&secret).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "the file the link points at must keep its mode"
        );
    }

    /// A target its owner left unreadable is answered, never silently skipped.
    ///
    /// The no-follow pair opens the file before it chmods the descriptor, so it
    /// needs READ access where `chmod(2)` needed none. A caller that cannot open
    /// the target gets the error, and the mode it asked for is not reported as
    /// applied.
    #[cfg(unix)]
    #[test]
    fn a_target_left_unreadable_is_answered_rather_than_silently_skipped() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let key = tmp.path().join("id_ed25519");
        std::fs::write(&key, b"private").unwrap();
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o000)).unwrap();

        let declared = super::set_file_permissions_nofollow(&key, 0o600);
        let mode = std::fs::metadata(&key).unwrap().permissions().mode() & 0o777;
        if crate::is_root() {
            // An elevated run's open bypasses the mode bits, so the chmod lands.
            assert!(
                declared.is_ok(),
                "root must still set the mode, got {declared:?}"
            );
            assert_eq!(mode, 0o600);
        } else {
            assert!(
                declared.is_err(),
                "an unopenable target must refuse, got {declared:?}"
            );
            assert_eq!(mode, 0o000, "a refused chmod moves nothing");
        }
    }
}
