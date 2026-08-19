/// Create a symbolic link. On Unix, uses `std::os::unix::fs::symlink`.
/// On Windows, uses `symlink_file` or `symlink_dir` based on the source type.
/// If symlink creation fails on Windows due to insufficient privileges,
/// returns an error with guidance to enable Developer Mode or run as admin.
pub fn create_symlink(source: &std::path::Path, target: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        create_symlink_impl(source, target)
    }
    #[cfg(windows)]
    {
        use super::paths::PathDisplayExt;
        create_symlink_impl(source, target).map_err(|e| {
            if e.raw_os_error() == Some(1314) {
                // ERROR_PRIVILEGE_NOT_HELD
                return std::io::Error::new(
                    e.kind(),
                    format!(
                        "symlink creation requires Developer Mode or admin privileges: {} -> {}\n\
                         Enable Developer Mode: Settings > Update & Security > For developers",
                        source.posix(),
                        target.posix()
                    ),
                );
            }
            e
        })
    }
}

#[cfg(unix)]
fn create_symlink_impl(source: &std::path::Path, target: &std::path::Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(source, target)
}

#[cfg(windows)]
fn create_symlink_impl(source: &std::path::Path, target: &std::path::Path) -> std::io::Result<()> {
    if source.is_dir() {
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
/// on "this is no longer the file I opened" must not act the same way on "I
/// could not look".
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
    // that `GetFileInformationByHandle` will overwrite before we read it.
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
        // the whole of what a holder is asking about.
        std::fs::remove_file(&file).unwrap();
        std::fs::write(&file, b"y").unwrap();
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
}
