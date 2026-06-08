use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Cooperative-cancellation flag for a running apply.
///
/// A value of `0` means "not aborted"; any nonzero value is the intended
/// process exit code (128 + signal number by POSIX convention — `130` for
/// `SIGINT`, `143` for `SIGTERM`). The reconciler checks [`AbortFlag::aborted`]
/// before starting each atomic action and stops cleanly when set, so an
/// in-flight write is never torn.
///
/// This type is deliberately free of any signal/process dependency: it is just
/// an `Arc<AtomicUsize>`. The CLI boundary owns signal registration and stores
/// the code into the shared atomic via [`AbortFlag::raw`]; library code only
/// reads it.
#[derive(Debug, Clone, Default)]
pub struct AbortFlag {
    inner: Arc<AtomicUsize>,
}

impl AbortFlag {
    /// Create a not-aborted flag.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// The intended exit code if an abort has been requested, else `None`.
    pub fn aborted(&self) -> Option<u8> {
        match self.inner.load(Ordering::SeqCst) {
            0 => None,
            // A registered signal stores 128 + signum, which always fits a u8.
            // The clamp guards a hypothetical out-of-range test value.
            code => Some(u8::try_from(code).unwrap_or(u8::MAX)),
        }
    }

    /// Request an abort with the given exit code. Used by tests and by any
    /// non-signal cancellation path; the CLI signal handler writes the shared
    /// atomic directly via [`AbortFlag::raw`].
    pub fn set(&self, code: u8) {
        self.inner.store(code as usize, Ordering::SeqCst);
    }

    /// The shared atomic, for handing to an async-signal-safe registrar
    /// (`signal_hook::flag::register_usize`) at the CLI boundary.
    pub fn raw(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_flag_is_not_aborted() {
        let flag = AbortFlag::new();
        assert_eq!(flag.aborted(), None);
    }

    #[test]
    fn set_records_exit_code() {
        let flag = AbortFlag::new();
        flag.set(130);
        assert_eq!(flag.aborted(), Some(130));
    }

    #[test]
    fn clone_shares_the_same_atomic() {
        let flag = AbortFlag::new();
        let clone = flag.clone();
        flag.set(143);
        assert_eq!(clone.aborted(), Some(143));
    }

    #[test]
    fn raw_handle_observes_writes() {
        let flag = AbortFlag::new();
        let raw = flag.raw();
        raw.store(130, Ordering::SeqCst);
        assert_eq!(flag.aborted(), Some(130));
    }
}
