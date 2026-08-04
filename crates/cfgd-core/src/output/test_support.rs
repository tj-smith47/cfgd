//! Test-only helpers for the output module. Gated to `#[cfg(test)]` because
//! every consumer mutates process-global state (`console::set_colors_enabled`
//! and friends).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

/// Serializes colour-flag ownership across the test binary. Two guards asking
/// for opposite states (`set(true)` in a renderer test, `set(false)` in a
/// plain-text theme test) would otherwise interleave and each read the other's
/// value.
static COLOR_PIN: Mutex<()> = Mutex::new(());

/// Set while a [`ColorsEnabledGuard`] is alive. Read by
/// `Printer::with_format`, which otherwise clears both colour flags on every
/// structured-output construction — a write performed by ordinary production
/// code from hundreds of non-serial tests, so `serial_test::serial` cannot
/// fence it. Pinning is the fence: the guard owns the flags for its lifetime,
/// and concurrent `Printer` construction leaves them alone.
static COLORS_PINNED: AtomicBool = AtomicBool::new(false);

/// Whether a test currently owns the process-global colour flags.
pub(crate) fn colors_are_pinned() -> bool {
    COLORS_PINNED.load(Ordering::SeqCst)
}

/// Panic-safe RAII guard for the process-global
/// `console::set_colors_enabled` and `console::set_colors_enabled_stderr`
/// flags. Captures both prior states on construction and restores them on
/// drop so a panicking assertion does not leak a `colors_enabled=false` state
/// into a later test.
///
/// Both flags are tracked because callers that disable colors (e.g.
/// `Printer::with_format` under structured output, NO_COLOR, or TERM=dumb)
/// toggle both — a stdout-only guard would silently leak the stderr flag.
///
/// Never construct two of these on one thread: the guard holds an exclusive
/// process-wide lock, so re-entering it deadlocks.
pub(crate) struct ColorsEnabledGuard {
    prior_stdout: bool,
    prior_stderr: bool,
    _pin: MutexGuard<'static, ()>,
}

impl ColorsEnabledGuard {
    pub(crate) fn set(enabled: bool) -> Self {
        let pin = COLOR_PIN.lock().unwrap_or_else(|e| e.into_inner());
        let prior_stdout = console::colors_enabled();
        let prior_stderr = console::colors_enabled_stderr();
        console::set_colors_enabled(enabled);
        console::set_colors_enabled_stderr(enabled);
        COLORS_PINNED.store(true, Ordering::SeqCst);
        Self {
            prior_stdout,
            prior_stderr,
            _pin: pin,
        }
    }
}

impl Drop for ColorsEnabledGuard {
    fn drop(&mut self) {
        COLORS_PINNED.store(false, Ordering::SeqCst);
        console::set_colors_enabled(self.prior_stdout);
        console::set_colors_enabled_stderr(self.prior_stderr);
    }
}
