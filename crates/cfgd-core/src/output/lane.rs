//! `LaneHandle` — a concurrent worker's entire view of the terminal.
//!
//! When the reconciler runs package work in per-manager lanes, ambient depth
//! stops being usable: it is per-renderer state, so two lanes reading it would
//! interleave. The coordinator is what knows an action's depth, so it creates
//! the lane's view and hands the worker a handle that exposes nothing but
//! "push a line of child output". No status, no section, no depth.
//!
//! The handle carries the two renderings a lane can have, and the caller never
//! chooses between them:
//!
//! - **TTY, non-quiet** — a bounded [`OutputWindow`] repainting in place at the
//!   action's depth, exactly as a sequential step's window does.
//! - **Everything else** — the lines are CAPTURED, not streamed. Two lanes
//!   streaming into one log interleave line by line, which would make CI logs
//!   unreadable and every golden non-deterministic; the capture is written
//!   beneath the action's own status when the phase's tree is composed.
//!   `Verbosity::Quiet` captures nothing, dropping the body as it always has.
use std::sync::Mutex;

use super::window::OutputWindow;
use super::{Printer, Verbosity};

/// The half of a lane a provider reached from inside it can address.
///
/// A trait rather than the concrete handle because a `PackageContext` carries
/// one lifetime for everything it borrows: `LaneHandle<'p>` is invariant in
/// `'p` (its window sits behind a `Mutex`), so a plain reference could not be
/// shortened to the context's own borrow, while `&dyn LaneOutput` can.
pub trait LaneOutput {
    /// Feed one raw line of child output into the lane.
    fn push_line(&self, raw: &str);

    /// Run `cmd` with its output flowing into the lane, settling no status.
    fn run(&self, cmd: &mut std::process::Command) -> std::io::Result<super::CommandOutput>;
}

/// A lane worker's whole terminal surface. Built by
/// [`Printer::lane_at`] on the coordinator thread and moved into the worker.
pub(crate) struct LaneHandle<'p> {
    /// The live repainting view, on a TTY. `None` off one, where
    /// [`LaneHandle::captured`] is the whole rendering.
    window: Option<Mutex<OutputWindow<'p>>>,
    /// Lines held back for the phase-close dump. `None` under
    /// `Verbosity::Quiet` and whenever a window is live, so the two renderings
    /// can never both produce a body for one action.
    captured: Option<Mutex<Vec<String>>>,
    /// The window draws on a line the coordinator owns — a
    /// [`super::live_row::LiveRow`] it will settle in place — so closing the
    /// lane releases that line instead of clearing it.
    borrowed_line: bool,
}

impl LaneOutput for LaneHandle<'_> {
    /// `&self` rather than `&mut self` because the handle reaches a package
    /// manager through the shared `PackageContext`. The interior mutex is
    /// uncontended by construction — one worker owns one lane.
    fn push_line(&self, raw: &str) {
        if let Some(window) = &self.window {
            window
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push_line(raw);
        }
        if let Some(captured) = &self.captured {
            captured
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(raw.to_string());
        }
    }

    /// Its stdout and stderr feed [`LaneOutput::push_line`] as they arrive, and
    /// the full capture comes back for the caller to post-process.
    ///
    /// Settles no status line of its own — in a lane the action's one line is
    /// always the coordinator's, written when the phase's tree is composed —
    /// and takes no label, because the lane's own label already names the
    /// action every command inside it belongs to.
    fn run(&self, cmd: &mut std::process::Command) -> std::io::Result<super::CommandOutput> {
        super::process::run_command_in_lane(cmd, self)
    }
}

impl<'p> LaneHandle<'p> {
    /// Collapse the lane and hand back the body its action's status line
    /// should carry, which is empty whenever the window already showed it.
    pub(crate) fn finish(self) -> Vec<String> {
        if let Some(window) = self.window {
            let window = window.into_inner().unwrap_or_else(|e| e.into_inner());
            if self.borrowed_line {
                window.release();
            } else {
                window.finish_silent();
            }
        }
        self.captured
            .map(|c| c.into_inner().unwrap_or_else(|e| e.into_inner()))
            .unwrap_or_default()
    }
}

impl Printer {
    /// Open a lane's terminal view at `depth`.
    ///
    /// `depth` is passed rather than inherited: a worker must never read
    /// ambient depth, which is per-renderer state that two lanes would
    /// interleave.
    #[must_use]
    pub(crate) fn lane_at(&self, depth: usize, label: impl Into<String>) -> LaneHandle<'_> {
        let live = self.live_bars();
        LaneHandle {
            window: live.then(|| Mutex::new(self.output_window_at(depth, label))),
            captured: (!live && self.verbosity() != Verbosity::Quiet)
                .then(|| Mutex::new(Vec::new())),
            borrowed_line: false,
        }
    }

    /// A lane whose live view is drawn on a row the coordinator owns.
    ///
    /// The capture half is the same as [`Printer::lane_at`]'s: a row exists
    /// only where there is a live region, so a caller holding one never
    /// captures — and one that has no row calls `lane_at` and does.
    #[must_use]
    pub(crate) fn lane_on_row<'p>(
        &self,
        row: &super::live_row::LiveRow<'p>,
        label: impl Into<String>,
    ) -> LaneHandle<'p> {
        LaneHandle {
            window: Some(Mutex::new(row.window(label))),
            captured: None,
            borrowed_line: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::super::{Printer, Verbosity};
    use super::LaneOutput;
    use crate::output::strip_ansi;

    /// A printer whose sink is a buffer and whose live region, colour, and
    /// stdin-tty are all pinned OFF — the state a redirected run is in —
    /// asserted here rather than inherited from however the suite happened to
    /// be invoked. `for_test_at` already pins all three; this wrapper exists
    /// only to name the intent at each call site.
    fn capturing_printer(verbosity: Verbosity) -> (Printer, Arc<Mutex<String>>) {
        Printer::for_test_at(verbosity)
    }

    #[test]
    fn off_a_tty_a_lane_captures_instead_of_streaming() {
        // Two lanes streaming into one log interleave line by line; the capture
        // is what lets the coordinator write each lane's body in one block.
        let (printer, buf) = capturing_printer(Verbosity::Normal);
        let lane = printer.lane_at(2, "module:nvim · brew install neovim");
        lane.push_line("==> Downloading");
        lane.push_line("==> Pouring");
        let body = lane.finish();
        assert_eq!(body, vec!["==> Downloading", "==> Pouring"]);
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(
            out.is_empty(),
            "a lane must reach the sink only through the coordinator: {out:?}"
        );
    }

    #[test]
    fn quiet_captures_no_body_at_all() {
        let (printer, _buf) = capturing_printer(Verbosity::Quiet);
        let lane = printer.lane_at(2, "module:nvim · brew install neovim");
        lane.push_line("noise");
        assert!(lane.finish().is_empty(), "Quiet drops the body, as it does");
    }

    #[test]
    fn two_lanes_plus_status_writes_are_not_garbled() {
        // The subject is the repainting path, which only exists where the
        // printer has a live region. `for_test_with_live_bars` gives it one
        // without a terminal: a real `MultiProgress` drawing to a recorder,
        // and the printer's own sink writing into the same buffer, so the two
        // writers interleave in one stream exactly as they would on a tty.
        let (printer, buf) = Printer::for_test_with_live_bars();
        let lanes: Vec<_> = ["brew install neovim", "apt install tmux"]
            .iter()
            .map(|label| printer.lane_at(2, format!("module:x · {label}")))
            .collect();

        std::thread::scope(|scope| {
            for (index, lane) in lanes.iter().enumerate() {
                scope.spawn(move || {
                    for line in 0..20 {
                        lane.push_line(&format!("lane {index} line {line}"));
                    }
                });
            }
            for line in 0..20 {
                printer.status_simple(
                    crate::output::Role::Ok,
                    format!("settled status line {line:02}"),
                );
            }
        });
        for lane in lanes {
            lane.finish();
        }

        // The recorded stream is what a terminal would have shown: bar
        // repaints and settled lines, in the order the two writers reached it.
        // A settled line that a repaint cut in half is no longer present as
        // one string, so a whole-string count of exactly one is the
        // not-garbled property — and the two-digit index keeps line 1 from
        // matching inside line 10.
        let out = strip_ansi(&buf.lock().unwrap_or_else(|e| e.into_inner()));
        // Not vacuous: a lane that fell back to capturing would put nothing in
        // the stream at all, and every count below would still be 1.
        assert!(
            out.contains("lane 0 line") && out.contains("lane 1 line"),
            "both lanes must have repainted into the stream: {out:?}"
        );
        for line in 0..20 {
            let expected = format!("settled status line {line:02}");
            assert_eq!(
                out.matches(&expected).count(),
                1,
                "{expected:?} is missing, duplicated, or was cut by a repaint"
            );
        }
    }
}
