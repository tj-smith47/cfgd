//! The terminal cursor while a live region is drawing.
//!
//! indicatif repaints a spinner in place and leaves the cursor wherever the
//! last paint put it, so for the whole of every wait a solid block sat in the
//! right margin of the spinner's own row — on camera in every demo, on every
//! beat with a bar. The region hides the cursor when its first bar goes up and
//! shows it again when its last bar comes down (`LiveBarGuard`, the ONE seam
//! every bar is counted through), so no call site can forget either half.
//!
//! A cursor hidden by a process that then dies stays hidden in the shell that
//! launched it, which is why the hide also arms a SIGINT/SIGTERM hook that
//! writes the show sequence before the process goes. The hook is registered
//! through `signal_hook`'s registry, the same one tokio and the apply's
//! cooperative abort chain through, so it runs BESIDE those handlers rather
//! than replacing them; and it emulates the default disposition only when no
//! owner has [claimed](claim_termination_signals) the signal, because a claimed
//! signal is one somebody else is already turning into a clean shutdown.

use std::sync::atomic::{AtomicBool, Ordering};

/// The escape a VT100 terminal shows its cursor on. Written from the signal
/// hook with a raw `write(2)`, which is why it is bytes and not a `Term` call.
/// The hook is the only writer and it is `cfg(unix)`, so on Windows the
/// constant would be dead code — a `-D warnings` build error rather than a
/// warning.
#[cfg(unix)]
// style-gate-ok: cursor visibility, not styling — the live region owns it and
// a colourless run still has to get its cursor back.
const SHOW_CURSOR: &[u8] = b"\x1b[?25h";

/// Whether the real terminal's cursor is hidden right now. Read from the
/// signal hook, so it is an atomic rather than a field on any printer.
static HIDDEN: AtomicBool = AtomicBool::new(false);

/// Whether a cooperative handler owns SIGINT/SIGTERM for this process.
static CLAIMED: AtomicBool = AtomicBool::new(false);

/// Hide the terminal's cursor and arm the restore-on-signal hook.
pub(super) fn hide(term: &console::Term) {
    let _ = term.hide_cursor();
    HIDDEN.store(true, Ordering::SeqCst);
    arm_restore_hook();
}

/// Show the terminal's cursor again.
pub(super) fn show(term: &console::Term) {
    HIDDEN.store(false, Ordering::SeqCst);
    let _ = term.show_cursor();
}

/// Declare that this process turns SIGINT/SIGTERM into a clean shutdown of
/// its own (the daemon's `ShutdownSignals`, a server binary's
/// `await_shutdown_request`, the apply's cooperative abort, the MCP server's
/// listener). The cursor hook then restores the cursor and leaves the signal
/// to that owner; unclaimed, it also emulates the default disposition, which
/// is what would have happened had the hook never been armed.
///
/// Call it BEFORE the owner's own registration: the hook only reads the flag
/// when a signal arrives, so the order of the two calls cannot race the
/// delivery, but a claim made after a delivery has already been emulated is a
/// claim made too late.
pub fn claim_termination_signals() {
    CLAIMED.store(true, Ordering::SeqCst);
}

#[cfg(unix)]
fn arm_restore_hook() {
    static ARMED: std::sync::Once = std::sync::Once::new();
    ARMED.call_once(|| {
        for sig in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
            // SAFETY: the action performs only async-signal-safe operations —
            // two atomic loads, one atomic store, one raw `write(2)` on a
            // stack buffer, and `emulate_default_handler`, which signal_hook
            // documents as async-signal-safe. No allocation, no lock, no
            // reentrant I/O through `console`.
            let action = move || {
                if HIDDEN.swap(false, Ordering::SeqCst) {
                    // The result is deliberately unread: there is nothing
                    // a handler about to let the process die could do
                    // with a short write.
                    let _ = unsafe {
                        libc::write(
                            libc::STDERR_FILENO,
                            SHOW_CURSOR.as_ptr().cast(),
                            SHOW_CURSOR.len(),
                        )
                    };
                }
                if !CLAIMED.load(Ordering::SeqCst) {
                    let _ = signal_hook::low_level::emulate_default_handler(sig);
                }
            };
            let registered = unsafe { signal_hook::low_level::register(sig, action) };
            if let Err(e) = registered {
                tracing::debug!(signal = sig, error = %e, "cursor restore hook not registered");
            }
        }
    });
}

/// Windows raises no POSIX signal on Ctrl-C and `console` restores the cursor
/// through the console API on the next `show`; a process killed mid-spinner
/// keeps the OS's own behaviour, as every indicatif user there does.
#[cfg(not(unix))]
fn arm_restore_hook() {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The show sequence the hook writes is the one `console` itself writes,
    /// so a terminal that took the hide from `Term::hide_cursor` takes the
    /// show from the raw bytes. Windows arms no hook and writes no sequence.
    #[cfg(unix)]
    #[test]
    fn the_raw_show_sequence_is_consoles_own() {
        assert_eq!(SHOW_CURSOR, b"\x1b[?25h");
    }

    #[test]
    fn a_claim_is_read_back_by_the_hook_gate() {
        // The flag is process-global, so this only proves the write lands;
        // the branch it gates cannot run outside a signal delivery.
        claim_termination_signals();
        assert!(CLAIMED.load(Ordering::SeqCst));
    }
}
