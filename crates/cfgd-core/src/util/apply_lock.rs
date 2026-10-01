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
///
/// Deliberately NOT `sources.lock`: the SHA lockfile beside the user's config
/// (`sources/lockfile.rs`) already owns that name, and two unrelated files
/// sharing it invites the wrong one being inspected or deleted. The cache
/// directory the file sits in already says what this lock is for.
pub const SOURCE_CACHE_LOCK_FILENAME: &str = "cache.lock";

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
        // Through the HELD handle, never through the path. The path can name a
        // different file by now (the lock file removed by a user wiping the
        // cache, and re-created by the next process), and truncating THAT one
        // erases a live holder's record — or, with `fs::write`, plants an
        // unlocked file at the path for a third process to lock while the
        // orphan is still held.
        if let Err(e) = held_file(&self._file).set_len(0) {
            tracing::debug!(path = ?self._path, error = %e, "failed to clear lock PID on drop");
        }
    }
}

/// The plain `File` inside a platform lock handle.
///
/// One body for both platforms: on Unix the `&Flock<File>` reaches `&File`
/// through `Deref`, on Windows it already is one.
fn held_file(lock: &LockFile) -> &std::fs::File {
    lock
}

/// Record this process's PID in the locked file, through the handle that holds
/// the lock.
///
/// Addressed by handle rather than by path for the reason [`acquire_lock_at`]
/// re-checks identity at all: a path-addressed write does not inherit the
/// identity the re-check established, so it can land in a file this process
/// does not hold. The write goes through a `try_clone` of the held handle (a
/// second descriptor over the same open file description) rather than through
/// `Flock`'s own `DerefMut`: a write through the `DerefMut` path was observed
/// dropped on macOS ARM64, and the dup keeps the write on a plain `File` code
/// path. Whether that avoids the dropped write there is evidence only the
/// real-OS runs can give; the exclusion itself is unaffected either way.
fn record_pid(lock: &LockFile) -> errors::Result<()> {
    use std::io::{Seek, Write};
    let mut file = held_file(lock).try_clone()?;
    file.set_len(0)?;
    file.rewind()?;
    file.write_all(pid_record().as_bytes())?;
    file.sync_all()?;
    Ok(())
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
        await_blocking_source_acquires(1, timeout)
    }

    /// Block until `wanted` threads are waiting on the source lock at once.
    ///
    /// The counting form is what lets a test put two contenders in ONE window
    /// before the holder releases: released on the single-waiter signal, the
    /// second contender may not have reached the acquire yet, and the two run
    /// one after another instead of racing.
    pub fn await_blocking_source_acquires(wanted: usize, timeout: std::time::Duration) -> bool {
        let (count, signal) = &*GATE;
        let guard = count.lock().unwrap_or_else(PoisonError::into_inner);
        let (guard, _) = signal
            .wait_timeout_while(guard, timeout, |count| *count < wanted)
            .unwrap_or_else(PoisonError::into_inner);
        *guard >= wanted
    }
}

#[cfg(test)]
pub use blocking_witness::{await_blocking_source_acquire, await_blocking_source_acquires};

/// Fault injection for the identity re-check, so the exhaustion arm can be
/// driven without a second process racing deletions in a loop.
#[cfg(test)]
mod stale_injection {
    use std::cell::Cell;

    thread_local! {
        static FORCED: Cell<usize> = const { Cell::new(0) };
    }

    /// Make the next `count` identity re-checks on THIS thread report the
    /// locked file as no longer the one the path names.
    pub fn force_stale_lock_rechecks(count: usize) {
        FORCED.with(|forced| forced.set(count));
    }

    pub(super) fn take_forced_stale() -> bool {
        FORCED.with(|forced| {
            let remaining = forced.get();
            if remaining == 0 {
                return false;
            }
            forced.set(remaining - 1);
            true
        })
    }
}

#[cfg(test)]
pub use stale_injection::force_stale_lock_rechecks;

/// Fault injection for the lock file's open, so the transient-retry arm runs
/// on every platform without reproducing a Windows delete-pending window.
#[cfg(test)]
mod open_injection {
    use std::cell::Cell;

    thread_local! {
        static FORCED: Cell<(usize, std::io::ErrorKind)> =
            const { Cell::new((0, std::io::ErrorKind::Other)) };
        static ATTEMPTS: Cell<usize> = const { Cell::new(0) };
    }

    /// Make the next `count` lock-file opens on THIS thread fail with `kind`
    /// before they touch the filesystem, and restart the attempt count.
    pub fn force_open_failures(count: usize, kind: std::io::ErrorKind) {
        FORCED.with(|forced| forced.set((count, kind)));
        ATTEMPTS.with(|attempts| attempts.set(0));
    }

    /// How many attempts THIS thread's transient retries (a lock-file open or
    /// a retried directory create) have made since the last
    /// [`force_open_failures`].
    pub fn retry_attempts() -> usize {
        ATTEMPTS.with(Cell::get)
    }

    pub(super) fn count_attempt() {
        ATTEMPTS.with(|attempts| attempts.set(attempts.get() + 1));
    }

    pub(super) fn next_open() -> std::io::Result<()> {
        FORCED.with(|forced| {
            let (remaining, kind) = forced.get();
            if remaining == 0 {
                return Ok(());
            }
            forced.set((remaining - 1, kind));
            Err(std::io::Error::from(kind))
        })
    }
}

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

/// The attempt budget of each of the two retry loops a lock acquire runs.
///
/// The loops are bounded separately. [`acquire_lock_at`] re-opens a lock file
/// that vanished, or was replaced, between the open and the lock, at most this
/// many times. Each of those opens runs its own transient retry
/// ([`retry_transient_open`]) with the same budget. The worst case for one
/// acquire is therefore this number squared in opens, and about two seconds of
/// backoff (eight full rounds of [`stale_retry_backoff`]).
///
/// One retry covers the real case: somebody removed the lock file (or the
/// directory holding it) while a contender was blocked on it. The remaining
/// attempts make a repeating removal end in an error after a bounded number of
/// tries.
pub(crate) const STALE_LOCK_ATTEMPTS: usize = 8;

/// How long a re-open waits before its next attempt: doubles from a few
/// milliseconds, capped well under a second.
///
/// A plain removal re-opens cleanly at once, so the first delay is short. The
/// Windows delete-pending window is different: it lasts until the deleter's
/// LAST handle closes, so back-to-back retries all land inside one window and
/// the attempt budget buys nothing. The backoff gives that handle time to
/// close. It is a chance, not a guarantee; a window outliving the whole budget
/// still surfaces the real io error.
fn stale_retry_backoff(attempt: usize) -> std::time::Duration {
    const BASE_MS: u64 = 4;
    const CAP_MS: u64 = 64;
    // 4 << 6 already clears the cap, so wider shifts cannot change the answer
    // and capping them keeps the shift in range for any attempt count.
    let doublings = attempt.saturating_sub(1).min(6) as u32;
    std::time::Duration::from_millis((BASE_MS << doublings).min(CAP_MS))
}

/// Acquire an exclusive whole-file lock at `lock_path`, on the file that
/// `lock_path` still names when the lock is granted.
///
/// The identity re-check is what keeps a REMOVED lock file from splitting the
/// section in two. Nothing in cfgd deletes one, but a user wiping a cache
/// directory does, and both platforms allow it while handles are open (`flock`
/// and `LockFileEx` lock an open FILE, not a path, and Rust's Windows opens
/// carry `FILE_SHARE_DELETE`). A contender blocked on the removed file would
/// otherwise wake holding an exclusive lock on an orphan nothing can open
/// again, while the next process creates a fresh file at the same path and
/// locks that one: two holders in one section, the interleaving the lock exists
/// to prevent. Re-opening on a mismatch settles it — the holder is whoever
/// holds the file the path currently names.
///
/// A removal takes the lock file's DIRECTORY with it as often as not, so the
/// re-open recreates the directory too (in [`open_lock_file`]), so waiting
/// politely never fails the contender with `ENOENT`.
///
/// Exhausting the attempts reports
/// [`errors::StateError::LockFileUnstable`] rather than handing back a guard
/// over a file the path no longer names: that guard would be the very
/// double-holder state the re-check exists to prevent. Deliberately NOT the
/// held-lock error: nobody is known to hold anything, so the caller must not
/// be sent looking for a holder, and [`acquire_source_lock`] must not read
/// the exhaustion as contention and announce a wait for it.
fn acquire_lock_at(lock_path: &std::path::Path, wait: LockWait) -> errors::Result<FileLockGuard> {
    let mut attempt = 1;
    loop {
        let last_attempt = attempt >= STALE_LOCK_ATTEMPTS;
        let locked = lock_file_at(lock_path, wait)?;
        let current = locked_file_is_current(&locked, lock_path);
        #[cfg(test)]
        let current = current && !stale_injection::take_forced_stale();
        if current {
            record_pid(&locked)?;
            return Ok(FileLockGuard {
                _file: locked,
                _path: lock_path.to_path_buf(),
            });
        }
        // Dropped bare, never through `FileLockGuard`, whose drop would clear a
        // PID record this process does not own.
        drop(locked);
        if last_attempt {
            return Err(errors::StateError::LockFileUnstable {
                path: lock_path.to_path_buf(),
            }
            .into());
        }
        attempt += 1;
    }
}

/// Whether an open failure is worth re-trying: the lock file or its directory
/// was removed (`NotFound`), or the open landed in a Windows delete-pending
/// window. A delete-pending lock FILE refuses the open with `PermissionDenied`.
/// A delete-pending DIRECTORY fails the re-create with `AlreadyExists`: its
/// `mkdir` finds the name taken, and `create_dir_all` accepts that only when
/// `is_dir()` holds, which it does not for a directory nothing can open.
///
/// The retry cannot tell delete-pending from a genuine EACCES; the backoff
/// between attempts gives a pending delete time to finish, and a denial that
/// outlives the budget surfaces as the io error it is. A regular file where a
/// directory belongs never reaches this question: [`create_dir_all_once`]
/// reports it as `NotADirectory`.
fn is_transient_open_error(io: &std::io::Error) -> bool {
    matches!(
        io.kind(),
        std::io::ErrorKind::NotFound
            | std::io::ErrorKind::PermissionDenied
            | std::io::ErrorKind::AlreadyExists
    )
}

// Acquire an exclusive whole-file lock via `flock()` (`LOCK_EX`, plus
// `LOCK_NB` under `LockWait::Refuse`): `StateError::ApplyLockHeld` when another
// holder has it and the caller refuses to wait.
#[cfg(unix)]
fn lock_file_at(lock_path: &std::path::Path, wait: LockWait) -> errors::Result<LockFile> {
    let file = open_lock_file(lock_path)?;

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

/// Open (creating if absent) the file a lock is taken on, re-creating its
/// directory first.
///
/// The directory matters on a RE-open: a removal that took the lock file is
/// usually a removal of the directory holding it, and a contender that came
/// back to find neither would fail with `ENOENT` while doing everything right.
///
/// The directory and the file are retried as one step, because a removal can
/// take the directory again between the two.
fn open_lock_file(lock_path: &std::path::Path) -> errors::Result<std::fs::File> {
    let file = retry_transient_open(|| {
        #[cfg(test)]
        open_injection::next_open()?;
        if let Some(parent) = lock_path.parent() {
            create_dir_all_once(parent)?;
        }
        std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)
    })?;
    Ok(file)
}

/// Run `op` until it succeeds, fails with an error [`is_transient_open_error`]
/// rejects, or has run [`STALE_LOCK_ATTEMPTS`] times, sleeping
/// [`stale_retry_backoff`] between attempts.
///
/// The backoff is what gives a retry a chance at the Windows delete-pending
/// windows: one clears only when the deleter's last handle closes, and
/// back-to-back attempts all land inside it.
fn retry_transient_open<T>(mut op: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let mut attempt = 1;
    loop {
        #[cfg(test)]
        open_injection::count_attempt();
        match op() {
            Err(e) if attempt < STALE_LOCK_ATTEMPTS && is_transient_open_error(&e) => {
                std::thread::sleep(stale_retry_backoff(attempt));
                attempt += 1;
            }
            done => return done,
        }
    }
}

/// `create_dir_all`, except that a regular file or a dangling symlink standing
/// at `dir` or at one of its ancestors fails as `NotADirectory` naming it.
///
/// `create_dir_all` reports a file or a dangling symlink at `dir` itself as
/// `AlreadyExists` on every platform, and one at an ancestor as
/// `AlreadyExists` on Windows (Unix already reports `NotADirectory` there).
/// `AlreadyExists` is also what a Windows delete-pending directory produces,
/// and only that case is worth waiting out. The walk up from `dir` tells them
/// apart at the first entry it can see:
/// - readable and a directory: nothing is in the way, so the error stays as it
///   is and the retry may wait;
/// - readable and anything else: a file is in the way;
/// - unreadable, but a symlink itself: a dangling symlink is in the way;
/// - unreadable and no symlink: a delete-pending directory (it cannot be read
///   at all) or nothing yet, so the walk moves on to the parent.
fn create_dir_all_once(dir: &std::path::Path) -> std::io::Result<()> {
    let Err(e) = std::fs::create_dir_all(dir) else {
        return Ok(());
    };
    if e.kind() != std::io::ErrorKind::AlreadyExists {
        return Err(e);
    }
    let in_the_way = |path: &std::path::Path, what: &str| {
        std::io::Error::new(
            std::io::ErrorKind::NotADirectory,
            format!("{} {what}", path.display()),
        )
    };
    for ancestor in dir.ancestors() {
        match std::fs::metadata(ancestor) {
            Ok(meta) if meta.is_dir() => break,
            Ok(_) => return Err(in_the_way(ancestor, "exists and is not a directory")),
            Err(_) if std::fs::symlink_metadata(ancestor).is_ok_and(|m| m.is_symlink()) => {
                return Err(in_the_way(ancestor, "is a symlink to no directory"));
            }
            Err(_) => {}
        }
    }
    Err(e)
}

/// Create `dir` and its ancestors, waiting out a Windows delete-pending
/// directory the way a lock file's own open does.
///
/// For a directory a lock is about to be taken in, created before the lock: a
/// removal racing that creation leaves the same transient state the lock's
/// re-open retries past, and failing on it at once would refuse a run the lock
/// would have let through.
pub(crate) fn create_dir_all_retrying(dir: &std::path::Path) -> std::io::Result<()> {
    retry_transient_open(|| create_dir_all_once(dir))
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

    let file = open_lock_file(lock_path)?;

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
    acquire_lock_at(&state_dir.join(APPLY_LOCK_FILENAME), LockWait::Refuse)
}

/// Acquire the exclusive source-cache lock at
/// `<cache_dir>/`[`SOURCE_CACHE_LOCK_FILENAME`].
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
    let lock_path = cache_dir.join(SOURCE_CACHE_LOCK_FILENAME);
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
    acquire_lock_at(&dir.join(format!("backup-{name}.lock")), LockWait::Refuse)
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::open_injection::{force_open_failures, retry_attempts};
    use std::io::ErrorKind;

    fn io(kind: ErrorKind) -> std::io::Error {
        std::io::Error::from(kind)
    }

    #[test]
    fn a_removed_lock_file_or_directory_is_transient() {
        assert!(is_transient_open_error(&io(std::io::ErrorKind::NotFound)));
    }

    #[test]
    fn a_delete_pending_lock_file_is_transient() {
        assert!(is_transient_open_error(&io(
            std::io::ErrorKind::PermissionDenied
        )));
    }

    #[test]
    fn a_delete_pending_lock_directory_is_transient() {
        assert!(is_transient_open_error(&io(
            std::io::ErrorKind::AlreadyExists
        )));
    }

    #[test]
    fn an_unrelated_open_failure_is_not_transient() {
        assert!(!is_transient_open_error(&io(
            std::io::ErrorKind::InvalidInput
        )));
    }

    #[test]
    fn an_apply_lock_acquire_succeeds_after_a_delete_pending_directory_clears() {
        let dir = tempfile::tempdir().unwrap();
        force_open_failures(1, ErrorKind::AlreadyExists);
        let held = acquire_apply_lock(dir.path());
        let attempts = retry_attempts();
        force_open_failures(0, ErrorKind::AlreadyExists);
        assert!(
            held.is_ok(),
            "one transient open failure is retried: {held:?}"
        );
        assert_eq!(attempts, 2);
    }

    #[test]
    fn a_delete_pending_directory_outliving_the_budget_surfaces_as_its_io_error() {
        let dir = tempfile::tempdir().unwrap();
        force_open_failures(STALE_LOCK_ATTEMPTS, ErrorKind::AlreadyExists);
        let held = acquire_apply_lock(dir.path());
        let attempts = retry_attempts();
        force_open_failures(0, ErrorKind::AlreadyExists);
        match held {
            Err(errors::CfgdError::Io(e)) => assert_eq!(e.kind(), ErrorKind::AlreadyExists),
            other => panic!("expected the io error itself, got {other:?}"),
        }
        assert_eq!(attempts, STALE_LOCK_ATTEMPTS);
    }

    #[test]
    fn a_source_lock_acquire_past_a_delete_pending_directory_never_announces_a_wait() {
        let dir = tempfile::tempdir().unwrap();
        force_open_failures(1, ErrorKind::AlreadyExists);
        let announced = std::cell::Cell::new(false);
        let held = acquire_source_lock(dir.path(), || announced.set(true));
        force_open_failures(0, ErrorKind::AlreadyExists);
        assert!(
            held.is_ok(),
            "one transient open failure is retried: {held:?}"
        );
        assert!(
            !announced.get(),
            "a transient open failure is not contention"
        );
    }

    #[test]
    fn a_regular_file_where_the_lock_directory_belongs_fails_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path().join("state");
        std::fs::write(&state_dir, b"").unwrap();
        force_open_failures(0, ErrorKind::AlreadyExists);
        let held = acquire_apply_lock(&state_dir);
        match held {
            Err(errors::CfgdError::Io(e)) => {
                assert_eq!(e.kind(), ErrorKind::NotADirectory);
                let named = state_dir.display().to_string();
                assert!(e.to_string().contains(&named), "{e}");
            }
            other => panic!("expected NotADirectory, got {other:?}"),
        }
        assert_eq!(retry_attempts(), 1, "a regular file is never waited out");
    }

    #[test]
    fn a_retrying_directory_create_refuses_a_regular_file_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("cache");
        std::fs::write(&file, b"").unwrap();
        force_open_failures(0, ErrorKind::AlreadyExists);
        let err = create_dir_all_retrying(&file).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::NotADirectory);
        assert_eq!(retry_attempts(), 1, "a regular file is never waited out");

        // Only Windows reports a file at an ANCESTOR as AlreadyExists; Unix
        // already answers NotADirectory from the mkdir itself.
        #[cfg(windows)]
        {
            force_open_failures(0, ErrorKind::AlreadyExists);
            let err = create_dir_all_retrying(&file.join("nested")).unwrap_err();
            assert_eq!(err.kind(), ErrorKind::NotADirectory);
            assert_eq!(retry_attempts(), 1, "a regular file is never waited out");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_where_the_lock_directory_belongs_fails_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path().join("state");
        std::os::unix::fs::symlink(dir.path().join("missing"), &state_dir).unwrap();
        force_open_failures(0, ErrorKind::AlreadyExists);
        match acquire_apply_lock(&state_dir) {
            Err(errors::CfgdError::Io(e)) => {
                assert_eq!(e.kind(), ErrorKind::NotADirectory);
                let named = state_dir.display().to_string();
                assert!(e.to_string().contains(&named), "{e}");
            }
            other => panic!("expected NotADirectory, got {other:?}"),
        }
        assert_eq!(
            retry_attempts(),
            1,
            "a dangling symlink is never waited out"
        );
    }
}
