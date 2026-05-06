use crate::errors;

/// Platform-specific lock file type.
/// Unix: `nix::fcntl::Flock` (safe RAII flock, unlocks on drop).
/// Windows: plain `File` (LockFileEx releases on handle close).
#[cfg(unix)]
type LockFile = nix::fcntl::Flock<std::fs::File>;
#[cfg(windows)]
type LockFile = std::fs::File;

/// RAII guard that releases the apply lock when dropped.
#[derive(Debug)]
pub struct ApplyLockGuard {
    _file: LockFile,
    _path: std::path::PathBuf,
}

impl Drop for ApplyLockGuard {
    fn drop(&mut self) {
        // Clear the PID so stale reads aren't confusing.
        // Lock is released when LockFile is dropped.
        if let Err(e) = self._file.set_len(0) {
            tracing::debug!(path = ?self._path, error = %e, "failed to clear apply-lock PID on drop");
        }
    }
}

/// Acquire an exclusive apply lock via `flock()`.
///
/// The lock file is created at `state_dir/apply.lock`. Uses non-blocking
/// `LOCK_EX | LOCK_NB` — returns `StateError::ApplyLockHeld` if another
/// process holds the lock. The lock is released automatically when the guard
/// is dropped.
#[cfg(unix)]
pub fn acquire_apply_lock(state_dir: &std::path::Path) -> errors::Result<ApplyLockGuard> {
    use std::io::Write;

    std::fs::create_dir_all(state_dir)?;
    let lock_path = state_dir.join("apply.lock");

    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)?;

    let mut locked = nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusiveNonblock)
        .map_err(|(_file, errno)| {
            if errno == nix::errno::Errno::EWOULDBLOCK {
                let holder = std::fs::read_to_string(&lock_path).unwrap_or_default();
                errors::CfgdError::from(errors::StateError::ApplyLockHeld {
                    holder: format!("pid {}", holder.trim()),
                })
            } else {
                errors::CfgdError::from(std::io::Error::from(errno))
            }
        })?;

    // Write our PID to the lock file
    locked.set_len(0)?;
    write!(locked, "{}", std::process::id())?;
    locked.sync_all()?;

    Ok(ApplyLockGuard {
        _file: locked,
        _path: lock_path,
    })
}

/// Acquire an exclusive apply lock via `LockFileEx`.
///
/// The lock file is created at `state_dir/apply.lock`. Uses
/// `LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY` — returns
/// `StateError::ApplyLockHeld` if another process holds the lock. The lock is
/// released automatically when the guard is dropped (file handle closed).
#[cfg(windows)]
pub fn acquire_apply_lock(state_dir: &std::path::Path) -> errors::Result<ApplyLockGuard> {
    use std::io::Write;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
    };

    std::fs::create_dir_all(state_dir)?;
    let lock_path = state_dir.join("apply.lock");

    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)?;

    let handle = file.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
    // SAFETY: `OVERLAPPED` is a plain-old-data struct of integers and a
    // handle field; the all-zero bit pattern is the documented "no event,
    // offset 0" initial value for synchronous-style LockFileEx calls.
    let mut overlapped: windows_sys::Win32::System::IO::OVERLAPPED = unsafe { std::mem::zeroed() };
    // SAFETY: `handle` is a valid, open, owned Win32 file handle derived
    // from `file`, which outlives the call. `&mut overlapped` points to a
    // stack-local, aligned, writable OVERLAPPED struct. The lock byte
    // range (offset 0, length 1) is fixed and valid. Non-blocking lock
    // (LOCKFILE_FAIL_IMMEDIATELY) avoids indefinite wait.
    let ret = unsafe {
        LockFileEx(
            handle,
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            1,
            0,
            &mut overlapped,
        )
    };
    if ret == 0 {
        let err = std::io::Error::last_os_error();
        // ERROR_LOCK_VIOLATION (33) = lock held by another process
        if err.raw_os_error() == Some(33) {
            let holder = std::fs::read_to_string(&lock_path).unwrap_or_default();
            return Err(errors::StateError::ApplyLockHeld {
                holder: format!("pid {}", holder.trim()),
            }
            .into());
        }
        return Err(err.into());
    }

    let mut f = file;
    f.set_len(0)?;
    write!(f, "{}", std::process::id())?;
    f.sync_all()?;

    Ok(ApplyLockGuard {
        _file: f,
        _path: lock_path,
    })
}
