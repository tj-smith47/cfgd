use crate::errors;

/// Filename of the apply mutex inside the state directory. The single source of
/// truth shared by [`acquire_apply_lock`] and `cfgd paths`, so the reported lock
/// path can never drift from the one actually acquired.
pub const APPLY_LOCK_FILENAME: &str = "apply.lock";

/// State-dir subdirectory holding per-resource lock files. Kept out of the
/// state-dir root so a lock file is never mistaken for one of the state dir's
/// own artifacts, and so the set of locks can be listed in one place.
const LOCKS_SUBDIR: &str = "locks";

/// Platform-specific lock file type.
/// Unix: `nix::fcntl::Flock` (safe RAII flock, unlocks on drop).
/// Windows: plain `File` (LockFileEx releases on handle close).
#[cfg(unix)]
type LockFile = nix::fcntl::Flock<std::fs::File>;
#[cfg(windows)]
type LockFile = std::fs::File;

/// Describe whoever holds `lock_path`, for the error a refused acquire returns.
///
/// The file is empty in two cases — a holder that has taken the lock but not
/// yet written its PID, and a non-cfgd holder (`flock(1)`) that never writes
/// one — and `pid ` with nothing after it reads like a bug in cfgd rather than
/// a lock held elsewhere.
fn holder_label(lock_path: &std::path::Path) -> String {
    let raw = std::fs::read_to_string(lock_path).unwrap_or_default();
    let raw = raw.trim();
    if raw.is_empty() {
        "unknown pid".to_string()
    } else {
        format!("pid {raw}")
    }
}

/// RAII guard that releases an exclusive file lock when dropped.
///
/// Shared by every cfgd mutex that is a whole-file lock: the machine-wide apply
/// lock and the per-unit backup locks. The guard body is identical for all of
/// them — only the path differs — so the acquire helpers below are thin wrappers
/// over one `acquire_lock_at`.
#[derive(Debug)]
pub struct FileLockGuard {
    _file: LockFile,
    _path: std::path::PathBuf,
}

impl Drop for FileLockGuard {
    fn drop(&mut self) {
        // Clear the PID so stale reads aren't confusing.
        // Lock is released when LockFile is dropped after this.
        if let Err(e) = std::fs::write(&self._path, b"") {
            tracing::debug!(path = ?self._path, error = %e, "failed to clear lock PID on drop");
        }
    }
}

// Acquire an exclusive whole-file lock via `flock()`, non-blocking
// (`LOCK_EX | LOCK_NB`): `StateError::ApplyLockHeld` when another holder has it.
//
// The PID is written via `std::fs::write` (a fresh open/write/close) rather
// than through the Flock fd because on macOS ARM64 writes through
// `Flock<File>`'s `DerefMut` are silently dropped (the flock exclusion is
// unaffected — `Flock<File>` still holds it).
#[cfg(unix)]
fn acquire_lock_at(lock_path: &std::path::Path) -> errors::Result<FileLockGuard> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;

    let locked = nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusiveNonblock)
        .map_err(|(_file, errno)| {
            if errno == nix::errno::Errno::EWOULDBLOCK {
                errors::CfgdError::from(errors::StateError::ApplyLockHeld {
                    holder: holder_label(lock_path),
                })
            } else {
                errors::CfgdError::from(std::io::Error::from(errno))
            }
        })?;

    std::fs::write(lock_path, std::process::id().to_string().as_bytes())?;

    Ok(FileLockGuard {
        _file: locked,
        _path: lock_path.to_path_buf(),
    })
}

// Acquire an exclusive whole-file lock via `LockFileEx`, non-blocking
// (`LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY`):
// `StateError::ApplyLockHeld` when another holder has it. The lock is released
// when the guard drops and the handle closes.
#[cfg(windows)]
fn acquire_lock_at(lock_path: &std::path::Path) -> errors::Result<FileLockGuard> {
    use std::io::Write;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
    };

    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;

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
            return Err(errors::StateError::ApplyLockHeld {
                holder: holder_label(lock_path),
            }
            .into());
        }
        return Err(err.into());
    }

    let mut f = file;
    f.set_len(0)?;
    write!(f, "{}", std::process::id())?;
    f.sync_all()?;

    Ok(FileLockGuard {
        _file: f,
        _path: lock_path.to_path_buf(),
    })
}

/// Acquire the machine-wide apply lock at `<state_dir>/apply.lock`.
///
/// Non-blocking: returns [`crate::errors::StateError::ApplyLockHeld`] naming the
/// holding PID when another `cfgd apply` (or the daemon's reconcile) already
/// holds it. Released when the returned guard drops.
pub fn acquire_apply_lock(state_dir: &std::path::Path) -> errors::Result<FileLockGuard> {
    std::fs::create_dir_all(state_dir)?;
    acquire_lock_at(&state_dir.join(APPLY_LOCK_FILENAME))
}

/// Acquire the exclusive lock for one `spec.backups[]` unit at
/// `<state_dir>/locks/backup-<name>.lock`.
///
/// Per-unit rather than global so two different backups still run
/// concurrently, and taken by every surface (CLI, apply, daemon timer) with no
/// opt-out: the backup engine's staging path is derived from the destination
/// alone, so two runs of ONE unit share `.<name>.partial` and the second run's
/// staging wipe lands inside the first run's in-flight tree. Retention pruning
/// has the same shape — it reads the run list, then deletes — so a concurrent
/// run can slip a row in between.
///
/// Non-blocking, like [`acquire_apply_lock`]: a held lock is reported as
/// [`crate::errors::StateError::ApplyLockHeld`] with the holding PID rather
/// than waited on, so a scheduled fire that collides with a hand-run is skipped
/// instead of queued behind it.
///
/// `name` is interpolated into the lock filename, so it is re-validated here
/// rather than trusted. Every in-tree caller passes a name
/// `config::validate_backup_specs` already accepted, but this is a `pub`
/// cfgd-core API and a `..`, `/`, or `.` slipping through would aim the lock
/// outside `locks/`.
pub fn acquire_backup_lock(
    state_dir: &std::path::Path,
    name: &str,
) -> errors::Result<FileLockGuard> {
    crate::config::validate_backup_name(name)?;
    let dir = state_dir.join(LOCKS_SUBDIR);
    std::fs::create_dir_all(&dir)?;
    acquire_lock_at(&dir.join(format!("backup-{name}.lock")))
}
