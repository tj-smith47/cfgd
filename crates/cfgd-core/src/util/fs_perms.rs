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

/// Check if two paths refer to the same file (same inode on Unix, same file index on Windows).
#[cfg(unix)]
pub fn is_same_inode(a: &std::path::Path, b: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(ma), Ok(mb)) => ma.ino() == mb.ino() && ma.dev() == mb.dev(),
        _ => false,
    }
}

#[cfg(windows)]
pub fn is_same_inode(a: &std::path::Path, b: &std::path::Path) -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::BY_HANDLE_FILE_INFORMATION;
    use windows_sys::Win32::Storage::FileSystem::GetFileInformationByHandle;

    fn file_info(path: &std::path::Path) -> Option<BY_HANDLE_FILE_INFORMATION> {
        let file = std::fs::File::open(path).ok()?;
        // SAFETY: `BY_HANDLE_FILE_INFORMATION` is a plain-old-data struct of
        // integer fields; the all-zero bit pattern is a valid initial value
        // that `GetFileInformationByHandle` will overwrite before we read it.
        let mut info = unsafe { std::mem::zeroed() };
        // SAFETY: `file.as_raw_handle()` returns a valid, open Win32 file
        // handle owned by `file`, which outlives the call. `&mut info`
        // points to sufficient, aligned, writable memory for the out
        // parameter. No aliasing: `info` is stack-local.
        let ret = unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut info) };
        if ret != 0 { Some(info) } else { None }
    }

    match (file_info(a), file_info(b)) {
        (Some(ia), Some(ib)) => {
            ia.dwVolumeSerialNumber == ib.dwVolumeSerialNumber
                && ia.nFileIndexHigh == ib.nFileIndexHigh
                && ia.nFileIndexLow == ib.nFileIndexLow
        }
        _ => false,
    }
}
