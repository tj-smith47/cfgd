use crate::errors;

/// Filename of the apply mutex inside the state directory. The single source of
/// truth shared by [`acquire_apply_lock`] and `cfgd paths`, so the reported lock
/// path can never drift from the one actually acquired.
pub const APPLY_LOCK_FILENAME: &str = "apply.lock";

/// State-dir subdirectory holding per-resource lock files. Kept out of the
/// state-dir root so a lock file is never mistaken for one of the state dir's
/// own artifacts, and so the set of locks can be listed in one place.
const LOCKS_SUBDIR: &str = "locks";

/// Filename of the source-cache mutex inside the sources cache directory.
///
/// Lives beside the per-source checkouts rather than in the state dir because
/// the cache is what it guards: a cache directory carried to another machine,
/// or wiped, takes its lock with it. `validate_source_name` rejects this name,
/// so no source's checkout can ever occupy the path.
pub const SOURCES_LOCK_FILENAME: &str = "sources.lock";

/// High half of the byte offset `LockFileEx` locks, i.e. the lock sits one byte
/// past 2^63 into the file.
///
/// `LockFileEx` ranges are **mandatory**, not advisory: while one process holds
/// a range exclusively, no other process may even READ those bytes. Locking
/// byte 0 — the obvious choice, and what this did — therefore made the PID
/// stored in the file unreadable by precisely the caller that needs it, the one
/// being refused, so every contended acquire reported `unknown pid`. The range
/// is parked far past any content the file will ever hold, which leaves the PID
/// readable while keeping the exclusion exactly as strict.
#[cfg(windows)]
const LOCK_RANGE_OFFSET_HIGH: u32 = 0x8000_0000;

/// Platform-specific lock file type.
/// Unix: `nix::fcntl::Flock` (safe RAII flock, unlocks on drop).
/// Windows: plain `File` (LockFileEx releases on handle close).
#[cfg(unix)]
type LockFile = nix::fcntl::Flock<std::fs::File>;
#[cfg(windows)]
type LockFile = std::fs::File;

/// Terminator the holder appends after its PID, and the marker `holder_label`
/// requires before it will believe what it read.
///
/// A holder truncates the file and then writes, so a contender reading inside
/// that window sees a *prefix* — `12` for a process whose real ID is `12345`.
/// A bare numeric parse accepts that prefix and names an unrelated process just
/// as confidently as a correct answer, which is strictly worse than admitting
/// the holder is unknown: nothing in the message tells the operator to distrust
/// it. Requiring the terminator makes the record self-delimiting, so a torn
/// read is detectable rather than plausible.
const PID_RECORD_TERMINATOR: char = '\n';

/// Describe whoever holds `lock_path`, for the error a refused acquire returns.
///
/// Reports a PID only for a complete, well-formed record: the holder's ID
/// followed by [`PID_RECORD_TERMINATOR`]. Everything else falls back to
/// `unknown pid` — an empty file (a holder that has taken the lock but not yet
/// written its PID, or a non-cfgd holder such as `flock(1)` that never writes
/// one), a torn prefix, and any content that is not a bare number.
fn holder_label(lock_path: &std::path::Path) -> String {
    let unknown = || "unknown pid".to_string();
    let raw = std::fs::read_to_string(lock_path).unwrap_or_default();
    match raw.strip_suffix(PID_RECORD_TERMINATOR) {
        Some(pid) => match pid.parse::<u32>() {
            Ok(pid) => format!("pid {pid}"),
            Err(_) => unknown(),
        },
        None => unknown(),
    }
}

/// The exact bytes a holder records in its lock file.
fn pid_record() -> String {
    format!("{}{PID_RECORD_TERMINATOR}", std::process::id())
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
        //
        // Truncate an existing file rather than `fs::write`, which creates one:
        // a holder may have deleted the lock it held (the failed-first-load
        // cache cleanup does exactly that), and re-creating it there would
        // leave a fresh empty file inside a directory that was just removed.
        let cleared = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&self._path);
        if let Err(e) = cleared {
            tracing::debug!(path = ?self._path, error = %e, "failed to clear lock PID on drop");
        }
    }
}

/// The observation that a thread has reached a [`LockWait::Block`] acquire and
/// is waiting on the holder.
///
/// It exists for the same reason [`crate::test_helpers::await_queued_path_writer`]
/// does: "a contender is waiting" is a state no channel between the two threads
/// can report, because the waiting thread is inside a syscall and runs no code
/// of its own until the lock is free. Without it a test can only observe that
/// the acquire eventually SUCCEEDED, which a non-blocking acquire also does the
/// moment the holder happens to release first — so the blocking arm can be
/// deleted outright and the test still passes.
#[cfg(test)]
mod blocking_witness {
    use std::sync::{Condvar, LazyLock, Mutex, PoisonError};

    static GATE: LazyLock<(Mutex<usize>, Condvar)> =
        LazyLock::new(|| (Mutex::new(0), Condvar::new()));

    /// Counts down when the acquire it was created for returns, however it
    /// returns, so one test's waiter cannot be mistaken for the next's.
    pub(super) struct Waiting;

    impl Drop for Waiting {
        fn drop(&mut self) {
            let (count, signal) = &*GATE;
            let mut count = count.lock().unwrap_or_else(PoisonError::into_inner);
            *count = count.saturating_sub(1);
            signal.notify_all();
        }
    }

    pub(super) fn entering_blocking_acquire() -> Waiting {
        let (count, signal) = &*GATE;
        let mut guard = count.lock().unwrap_or_else(PoisonError::into_inner);
        *guard += 1;
        signal.notify_all();
        Waiting
    }

    /// Block until some thread is inside a blocking source-lock acquire.
    /// `timeout` is a deadlock escape, never a timing assertion: the answer is
    /// the returned bool, and a caller asserts on that.
    pub fn await_blocking_source_acquire(timeout: std::time::Duration) -> bool {
        let (count, signal) = &*GATE;
        let guard = count.lock().unwrap_or_else(PoisonError::into_inner);
        let (guard, _) = signal
            .wait_timeout_while(guard, timeout, |count| *count == 0)
            .unwrap_or_else(PoisonError::into_inner);
        *guard > 0
    }
}

#[cfg(test)]
pub use blocking_witness::await_blocking_source_acquire;

/// What a contended acquire does about the holder.
///
/// The machine-wide mutexes ([`acquire_apply_lock`], [`acquire_backup_lock`])
/// [`Refuse`](LockWait::Refuse), so a scheduled fire colliding with a hand-run
/// is skipped rather than queued behind it. The source-cache mutex
/// [`Block`](LockWait::Block)s instead: its critical section is short, both
/// contenders want the same end state, and refusing would turn a benign
/// overlap between `cfgd sync` and `cfgd apply` into a failed run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LockWait {
    /// Report [`errors::StateError::ApplyLockHeld`] immediately.
    Refuse,
    /// Wait for the holder to release. Bounded by the holder's own lifetime:
    /// the OS drops both `flock` and `LockFileEx` when the process exits, so a
    /// crashed holder cannot strand a waiter.
    Block,
}

/// How many times an acquire re-opens a lock file that was deleted between the
/// open and the lock.
///
/// One retry is the real case (a holder that removed the file it locked, which
/// is what a failed first-ever source load does to an empty cache root). The
/// remaining attempts exist so a pathological sequence of deletions ends in an
/// error rather than a spin.
const STALE_LOCK_ATTEMPTS: usize = 8;

/// Acquire an exclusive whole-file lock at `lock_path`, on the file that
/// `lock_path` still names when the lock is granted.
///
/// The identity re-check is what makes deleting a lock file safe. Both
/// `flock` and `LockFileEx` lock an OPEN FILE, not a path, and both platforms
/// let a file be unlinked while handles to it are open (Rust's Windows opens
/// carry `FILE_SHARE_DELETE`). So a contender that blocks on the lock, and
/// whose holder then removes the file, wakes up holding an exclusive lock on an
/// orphan inode that no later process can ever open — while the next process
/// creates a fresh file at the same path and locks that one instead. Two
/// processes then hold "the" lock at once, which is the exact interleaving the
/// lock exists to prevent. Re-opening on a mismatch closes it: the winner is
/// whoever holds the file the path currently names.
fn acquire_lock_at(lock_path: &std::path::Path, wait: LockWait) -> errors::Result<FileLockGuard> {
    let mut attempt = 1;
    loop {
        let mut locked = lock_file_at(lock_path, wait)?;
        if attempt < STALE_LOCK_ATTEMPTS && !locked_file_is_current(&locked, lock_path) {
            // Dropped bare, not through `FileLockGuard`: the guard's drop
            // clears the PID record by PATH, which now names somebody else's
            // file.
            drop(locked);
            attempt += 1;
            continue;
        }
        record_pid(&mut locked, lock_path)?;
        return Ok(FileLockGuard {
            _file: locked,
            _path: lock_path.to_path_buf(),
        });
    }
}

// Acquire an exclusive whole-file lock via `flock()` (`LOCK_EX`, plus
// `LOCK_NB` under `LockWait::Refuse`): `StateError::ApplyLockHeld` when another
// holder has it and the caller refuses to wait.
#[cfg(unix)]
fn lock_file_at(lock_path: &std::path::Path, wait: LockWait) -> errors::Result<LockFile> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;

    let arg = match wait {
        LockWait::Refuse => nix::fcntl::FlockArg::LockExclusiveNonblock,
        LockWait::Block => nix::fcntl::FlockArg::LockExclusive,
    };
    nix::fcntl::Flock::lock(file, arg).map_err(|(_file, errno)| {
        if errno == nix::errno::Errno::EWOULDBLOCK {
            errors::CfgdError::from(errors::StateError::ApplyLockHeld {
                holder: holder_label(lock_path),
            })
        } else {
            errors::CfgdError::from(std::io::Error::from(errno))
        }
    })
}

// The PID is written via `std::fs::write` (a fresh open/write/close) rather
// than through the Flock fd because on macOS ARM64 writes through
// `Flock<File>`'s `DerefMut` are silently dropped (the flock exclusion is
// unaffected — `Flock<File>` still holds it).
#[cfg(unix)]
fn record_pid(_file: &mut LockFile, lock_path: &std::path::Path) -> errors::Result<()> {
    std::fs::write(lock_path, pid_record().as_bytes())?;
    Ok(())
}

/// Whether the locked file is still the one `lock_path` names.
#[cfg(unix)]
fn locked_file_is_current(file: &LockFile, lock_path: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (file.metadata(), std::fs::metadata(lock_path)) {
        (Ok(held), Ok(named)) => held.ino() == named.ino() && held.dev() == named.dev(),
        _ => false,
    }
}

// Acquire an exclusive whole-file lock via `LockFileEx`
// (`LOCKFILE_EXCLUSIVE_LOCK`, plus `LOCKFILE_FAIL_IMMEDIATELY` under
// `LockWait::Refuse`): `StateError::ApplyLockHeld` when another holder has it
// and the caller refuses to wait. The lock is released when the guard drops
// and the handle closes.
#[cfg(windows)]
fn lock_file_at(lock_path: &std::path::Path, wait: LockWait) -> errors::Result<LockFile> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
    };
    use windows_sys::Win32::System::IO::{OVERLAPPED, OVERLAPPED_0, OVERLAPPED_0_0};

    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;

    let handle = file.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
    let mut overlapped = OVERLAPPED {
        Anonymous: OVERLAPPED_0 {
            Anonymous: OVERLAPPED_0_0 {
                Offset: 0,
                OffsetHigh: LOCK_RANGE_OFFSET_HIGH,
            },
        },
        ..Default::default()
    };
    let flags = match wait {
        LockWait::Refuse => LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
        LockWait::Block => LOCKFILE_EXCLUSIVE_LOCK,
    };
    // SAFETY: `handle` is a valid, open, owned Win32 file handle derived
    // from `file`, which outlives the call. `&mut overlapped` points to a
    // stack-local, aligned, writable OVERLAPPED struct. The lock byte
    // range (one byte at `LOCK_RANGE_OFFSET_HIGH << 32`) is fixed and valid.
    // A `Block` acquire waits inside this call until the holder releases; the
    // holder is another cfgd process, and the OS releases its lock on exit.
    let ret = unsafe { LockFileEx(handle, flags, 0, 1, 0, &mut overlapped) };
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

    Ok(file)
}

#[cfg(windows)]
fn record_pid(file: &mut LockFile, _lock_path: &std::path::Path) -> errors::Result<()> {
    use std::io::Write;
    file.set_len(0)?;
    write!(file, "{}", pid_record())?;
    file.sync_all()?;
    Ok(())
}

/// Whether the locked file is still the one `lock_path` names.
///
/// Windows has no inode, so identity is the volume serial plus the file index,
/// read from the HELD handle (the path's own entry may already be gone).
#[cfg(windows)]
fn locked_file_is_current(file: &LockFile, lock_path: &std::path::Path) -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    fn info_of(file: &std::fs::File) -> Option<BY_HANDLE_FILE_INFORMATION> {
        // SAFETY: `BY_HANDLE_FILE_INFORMATION` is a plain-old-data struct of
        // integer fields; the all-zero bit pattern is a valid initial value
        // that `GetFileInformationByHandle` overwrites before it is read.
        let mut info = unsafe { std::mem::zeroed() };
        // SAFETY: `file.as_raw_handle()` is a valid, open Win32 file handle
        // owned by `file`, which outlives the call. `&mut info` points to
        // sufficient, aligned, stack-local writable memory for the out
        // parameter, and nothing else aliases it.
        let ret = unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut info) };
        if ret != 0 { Some(info) } else { None }
    }

    let Some(held) = info_of(file) else {
        return false;
    };
    let Some(named) = std::fs::File::open(lock_path)
        .ok()
        .as_ref()
        .and_then(info_of)
    else {
        return false;
    };
    held.dwVolumeSerialNumber == named.dwVolumeSerialNumber
        && held.nFileIndexHigh == named.nFileIndexHigh
        && held.nFileIndexLow == named.nFileIndexLow
}

/// Acquire the machine-wide apply lock at `<state_dir>/apply.lock`.
///
/// Non-blocking: returns [`crate::errors::StateError::ApplyLockHeld`] naming the
/// holding PID when another `cfgd apply` (or the daemon's reconcile) already
/// holds it. Released when the returned guard drops.
pub fn acquire_apply_lock(state_dir: &std::path::Path) -> errors::Result<FileLockGuard> {
    std::fs::create_dir_all(state_dir)?;
    acquire_lock_at(&state_dir.join(APPLY_LOCK_FILENAME), LockWait::Refuse)
}

/// Acquire the exclusive source-cache lock at
/// `<cache_dir>/`[`SOURCES_LOCK_FILENAME`].
///
/// Held across a source's origin check, the discard of a mismatched checkout,
/// and the clone or fetch that replaces it. That sequence is a
/// check-then-act over a directory keyed by the source NAME alone, so two cfgd
/// processes composing different configs that name one source can otherwise
/// interleave: one process's fetch resolves `origin` after the other re-pointed
/// it, or a clone lands in a tree the other is removing.
///
/// Separate from the apply lock on purpose. A read path (`cfgd plan`,
/// `cfgd status`) must not be refused because an apply is running, and an apply
/// must not be refused because a `cfgd sync` is warming the cache.
///
/// Blocking rather than refusing: the critical section is one clone, both
/// contenders want the same end state, and a refusal would fail a run over an
/// overlap that resolves itself. `on_wait` is called at most once, only when a
/// holder is already in the section, so a caller can say so before the wait
/// begins rather than appearing to hang.
pub fn acquire_source_lock(
    cache_dir: &std::path::Path,
    on_wait: impl FnOnce(),
) -> errors::Result<FileLockGuard> {
    std::fs::create_dir_all(cache_dir)?;
    let lock_path = cache_dir.join(SOURCES_LOCK_FILENAME);
    match acquire_lock_at(&lock_path, LockWait::Refuse) {
        Err(errors::CfgdError::State(errors::StateError::ApplyLockHeld { .. })) => {
            on_wait();
            #[cfg(test)]
            let _waiting = blocking_witness::entering_blocking_acquire();
            acquire_lock_at(&lock_path, LockWait::Block)
        }
        other => other,
    }
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
    acquire_lock_at(&dir.join(format!("backup-{name}.lock")), LockWait::Refuse)
}
