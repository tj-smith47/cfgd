//! The `tracing` sink that cannot strand a live bar.
//!
//! A `tracing_subscriber::fmt` layer writing `std::io::stderr` writes the same
//! stream a live region repaints, and it knows nothing about the region: a
//! single `tracing::warn!` landing while a spinner is on screen leaves that
//! spinner's last paint on the terminal forever, because indicatif rewinds a
//! fixed number of rows and the rows moved underneath it.
//!
//! The filter is the volume control (the cfgd binary defaults to `warn`, and
//! each `-v` opens one more level); this writer is the guarantee for the events
//! that survive it. Every write is routed through the same `MultiProgress` the
//! renderer routes its own emissions through, so an event clears the bars,
//! writes its line, and lets them repaint beneath it.
//!
//! It is also a terminal writer that passes no renderer, so it carries the
//! renderer's fold itself: every event is folded through
//! [`super::cursor_safe`] before either destination sees it. A subscriber
//! wiring this writer therefore has to disable the formatter's own colours
//! (`.with_ansi(false)`) — the fold strips ANSI, so SGR the formatter emitted
//! would be eaten, and the level tint is not worth a second sanitation policy
//! that has to tell an `ESC [ 0 m` from an `ESC [ 2 K`. That obligation is not
//! left to whoever reads this: `every_subscriber_writes_through_a_folding_writer`
//! (`output/tests/fences.rs`) refuses a wiring that names this writer without
//! it, and refuses any subscriber in the workspace that names a different
//! writer without a stated reason.

use std::io::{self, Write};
use std::sync::{Arc, RwLock};

use super::Printer;
use super::renderer::LiveBarState;

/// The wall clock every human-readable cfgd subscriber stamps its events with:
/// the local time of day, `%H:%M:%S`.
///
/// Local rather than UTC, and time-of-day rather than a full instant, because
/// the reader is a person at the machine — someone tailing `cfgd daemon run`'s
/// log, or reading a warning a one-shot command emitted seconds ago. The
/// question they ask of a stamp is "how long ago", and a date they already know
/// plus an offset they have to apply answers it slower than the clock on their
/// own wall does.
///
/// The in-cluster binaries keep the formatter's default RFC 3339 UTC: their
/// events are collected, correlated across nodes and read long after the fact,
/// which is the opposite reader.
///
/// A clock that cannot be read leaves the field empty rather than failing the
/// event — `tracing_subscriber` substitutes `<unknown time>` for an `Err`, and
/// an event whose message was going to say something useful is worth more than
/// the stamp it lost. `chrono::Local` has no failing arm today; the `write!`
/// does.
#[derive(Clone, Copy, Debug, Default)]
pub struct LocalTimeOfDay;

impl tracing_subscriber::fmt::time::FormatTime for LocalTimeOfDay {
    fn format_time(&self, w: &mut tracing_subscriber::fmt::format::Writer<'_>) -> std::fmt::Result {
        write!(w, "{}", chrono::Local::now().format("%H:%M:%S"))
    }
}

/// Where a routed event goes, and the latch that says whether routing is still
/// working — the SAME latch every renderer writing this region consults, so a
/// terminal judged dead is dead for both writers rather than re-probed once per
/// event.
#[derive(Clone)]
struct LiveRoute {
    bars: indicatif::MultiProgress,
    live: Arc<LiveBarState>,
}

/// A `MakeWriter` bound to a [`Printer`]'s live region.
///
/// Built before the subscriber is installed and attached once the process
/// printer exists, because those two happen in that order: the subscriber has
/// to be live for anything the printer's own construction logs. Until
/// [`Self::attach`] is called — and on any printer that has no live region —
/// writes go straight to stderr, which is exactly what the subscriber did
/// before this existed.
#[derive(Clone, Default)]
pub struct LiveTracingWriter {
    route: Arc<RwLock<Option<LiveRoute>>>,
}

impl LiveTracingWriter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Route every later event through `printer`'s live region.
    ///
    /// Takes the `Printer` rather than its `MultiProgress` so no caller outside
    /// `output/` ever holds an indicatif handle (Hard Rule #1).
    pub fn attach(&self, printer: &Printer) {
        let mut slot = self.route.write().unwrap_or_else(|e| e.into_inner());
        *slot = Some(LiveRoute {
            bars: printer.multi_progress.clone(),
            live: printer.renderer.live.clone(),
        });
    }

    fn target(&self) -> Option<LiveRoute> {
        self.route
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .filter(|route| !route.bars.is_hidden() && !route.live.broken())
            .cloned()
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LiveTracingWriter {
    type Writer = LiveTracingSink;

    fn make_writer(&'a self) -> Self::Writer {
        LiveTracingSink {
            route: self.target(),
            buf: Vec::new(),
        }
    }
}

/// One event's worth of formatted bytes.
///
/// The bytes are held until the sink is flushed or dropped rather than written
/// as they arrive: the formatter writes an event in several calls, and clearing
/// and repainting the bars around each fragment would interleave the region
/// with a partial line.
pub struct LiveTracingSink {
    route: Option<LiveRoute>,
    buf: Vec<u8>,
}

impl LiveTracingSink {
    fn emit(&mut self) {
        self.emit_with(&mut io::stderr().lock());
    }

    /// The emission itself, with the fall-through destination supplied rather
    /// than opened, so the arm that runs when there is no live region — or
    /// when the region's terminal has just refused the write — can be read
    /// back instead of landing on the process's own stderr.
    fn emit_with(&mut self, fallback: &mut dyn Write) {
        if self.buf.is_empty() {
            return;
        }
        let bytes = std::mem::take(&mut self.buf);
        // Both arms below write the SAME folded text, because both are
        // terminal writers and neither passes a renderer. An event's fields
        // carry strings cfgd did not author — a module-declared file target, a
        // source name, a remote API's parse error — and a `\r` or an
        // `ESC [ 2 K` among them repaints or erases the line describing it.
        // `tracing_subscriber` escapes ESC and the C1 range inside the message
        // field alone, so `\r` and every `%`-formatted field value walk
        // through it untouched; the fold is what makes the whole line safe.
        // The subscribers wiring this writer pass `.with_ansi(false)`, so
        // there is no formatter SGR left for the fold to strip.
        let folded = super::cursor_safe(&String::from_utf8_lossy(&bytes));
        if let Some(route) = &self.route {
            // `println` supplies the line ending itself, and a trailing one
            // here would draw a blank row into the region on every event.
            match route.bars.println(folded.trim_end_matches('\n')) {
                Ok(()) => {
                    route.live.note_route_success();
                    return;
                }
                // The renderer's judgement, made from here: a kind that proves
                // the terminal gone latches at once, a transient refusal only
                // counts toward the latch.
                Err(e) => {
                    route.live.record_route_failure(&e);
                }
            }
        }
        // No region, or the region's terminal just refused the write: the
        // event still has to reach the user, so fall through to the stream the
        // subscriber wrote before any of this existed.
        let _ = fallback.write_all(folded.as_bytes());
        let _ = fallback.flush();
    }
}

impl Write for LiveTracingSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.emit();
        Ok(())
    }
}

impl Drop for LiveTracingSink {
    fn drop(&mut self) {
        self.emit();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::fmt::MakeWriter;

    /// A tracing event landing mid-spinner clears and repaints the region
    /// instead of writing over the top of it.
    ///
    /// The control arm is the write the subscriber used to make — the same
    /// bytes straight at the terminal — and it is what makes the subject arm's
    /// assertion non-vacuous: it strands the running label on the emulated
    /// screen, which is the only live capture that can see a paint the region
    /// never took back.
    #[test]
    fn a_tracing_event_does_not_strand_a_live_bar() {
        let raw_held = {
            let (printer, screen) = Printer::for_test_live_terminal(24, 100);
            let sp = printer.spinner("Fetching module index");
            // Joins the steady-tick thread, so this thread is the only writer
            // and the bar has already painted a frame — no sleep, no race.
            sp.bar.disable_steady_tick();
            printer.sink_stderr.write_line("WARN unsigned module");
            sp.finish_ok("Fetched module index");
            printer.flush();
            screen.contents()
        };
        assert!(
            raw_held.contains("Fetching module index"),
            "the control arm did not strand anything, so the subject arm below \
             proves nothing: {raw_held:?}"
        );

        let (printer, screen) = Printer::for_test_live_terminal(24, 100);
        let sp = printer.spinner("Fetching module index");
        sp.bar.disable_steady_tick();
        let writer = LiveTracingWriter::new();
        writer.attach(&printer);
        writer
            .make_writer()
            .write_all(b"WARN unsigned module\n")
            .expect("the sink buffers, so a write cannot fail");
        sp.finish_ok("Fetched module index");
        printer.flush();

        let held = screen.contents();
        assert!(
            !held.contains("Fetching module index"),
            "the running spinner's paint was stranded on the terminal: {held:?}"
        );
        assert_eq!(
            held.matches("WARN unsigned module").count(),
            1,
            "the event must land exactly once: {held:?}"
        );
        assert!(
            held.contains("Fetched module index"),
            "the settled line went missing: {held:?}"
        );
    }

    /// A `tracing` event carries strings cfgd did not author, and this writer
    /// is a terminal writer that passes no renderer: an unfolded `\r` walks
    /// the cursor back to column zero and an unfolded `ESC [ 2 K` erases the
    /// row, so what the region ends up holding is the tail of the event
    /// wearing the head's place.
    ///
    /// Read off the EMULATED SCREEN, which executes both, because a buffer
    /// capture would show the bytes rather than what they did. The fold is the
    /// DISPLAY policy, so the escape sequence is stripped and the lone `\r`
    /// is escaped into view — a routed event is not a screen anyone approves
    /// from.
    #[test]
    fn a_hostile_event_cannot_repaint_the_region_it_lands_in() {
        let (printer, screen) = Printer::for_test_live_terminal(24, 120);
        printer.status_simple(super::super::Role::Info, "an earlier line");
        let writer = LiveTracingWriter::new();
        writer.attach(&printer);
        writer
            .make_writer()
            .write_all(b"WARN cannot read ~/evil\r\x1b[2Kmodules/a for hashing\n")
            .expect("the sink buffers, so a write cannot fail");
        printer.flush();

        let held = screen.contents();
        let row = held
            .lines()
            .find(|l| l.contains("cannot read"))
            .unwrap_or_else(|| panic!("the event never landed; screen holds: {held:?}"));
        assert_eq!(
            row.trim_end(),
            "WARN cannot read ~/evil\\x0dmodules/a for hashing",
            "the event's control characters acted instead of showing: {held:?}"
        );
        assert!(
            held.contains("an earlier line"),
            "the event took a line that was already on screen with it: {held:?}"
        );
    }

    /// The same guarantee on the arm that runs when there is no live region to
    /// route through — an unattached writer, and a region whose terminal has
    /// latched. Both fall through to the stream, and the stream is a terminal
    /// too.
    #[test]
    fn the_fall_through_arm_writes_the_same_folded_line() {
        let mut sink = LiveTracingSink {
            route: None,
            buf: Vec::new(),
        };
        sink.write_all(b"WARN cannot read ~/evil\r\x1b[2Kmodules/a for hashing\n")
            .expect("the sink buffers, so a write cannot fail");
        let mut fallback = Vec::new();
        sink.emit_with(&mut fallback);
        assert_eq!(
            String::from_utf8_lossy(&fallback),
            "WARN cannot read ~/evil\\x0dmodules/a for hashing\n",
            "the fall-through arm wrote bytes that can move a cursor"
        );
    }

    /// A CRLF the formatter or a Windows-captured message brought with it is
    /// ONE line break, not a cursor move followed by one — and the trailing
    /// newline still comes off before the region draws the row, or every event
    /// leaves a blank line under itself.
    #[test]
    fn a_line_ending_survives_the_fold_as_a_line_ending() {
        let mut sink = LiveTracingSink {
            route: None,
            buf: Vec::new(),
        };
        sink.write_all(b"WARN a windows message\r\n")
            .expect("the sink buffers, so a write cannot fail");
        let mut fallback = Vec::new();
        sink.emit_with(&mut fallback);
        assert_eq!(
            String::from_utf8_lossy(&fallback),
            "WARN a windows message\n",
            "a CRLF was rendered as a visible escape instead of a line break"
        );

        let (printer, screen) = Printer::for_test_live_terminal(24, 120);
        let writer = LiveTracingWriter::new();
        writer.attach(&printer);
        writer
            .make_writer()
            .write_all(b"WARN a windows message\r\n")
            .expect("the sink buffers, so a write cannot fail");
        printer.flush();
        let held = screen.contents();
        assert_eq!(
            held.lines()
                .filter(|l| l.contains("a windows message"))
                .count(),
            1,
            "the event landed more than once: {held:?}"
        );
    }

    /// Which sink an event gets, for each of the three states the writer can be
    /// in: unattached (the subscriber is installed before the printer exists),
    /// attached to a live region, and attached to a printer whose region draws
    /// nowhere. Only the middle one is a routing target — the other two fall
    /// through to the stream the subscriber wrote before this type existed, and
    /// routing a `println` at a hidden region would swallow the event instead.
    ///
    /// The sinks are inspected, never written: a write here would land on the
    /// suite's own stderr.
    #[test]
    fn only_a_visible_region_becomes_a_routing_target() {
        let unattached = LiveTracingWriter::new();
        assert!(
            unattached.target().is_none(),
            "a fresh writer must have no region to route through"
        );
        assert!(
            unattached.make_writer().route.is_none(),
            "an unattached sink must fall through to stderr"
        );

        let (printer, _screen) = Printer::for_test_live_terminal(24, 100);
        let attached = LiveTracingWriter::new();
        attached.attach(&printer);
        assert!(
            attached.target().is_some(),
            "an attached writer routes through the printer's region"
        );
        assert!(
            attached.make_writer().route.is_some(),
            "each sink the subscriber makes inherits the region"
        );

        // A hidden draw target: the region exists but paints nothing, so a
        // `println` at it would consume the event silently.
        let (hidden, _buf) = Printer::for_test_live_scrollback();
        let over_hidden = LiveTracingWriter::new();
        over_hidden.attach(&hidden);
        assert!(
            over_hidden.target().is_none(),
            "a region that draws nowhere is not a routing target"
        );
    }

    /// The writer and the renderers share ONE judgement about ONE terminal: once
    /// the renderer's latch says the region stopped answering, the writer must
    /// stop routing events at it too, rather than re-discovering the dead
    /// terminal with a failed `println` per event.
    #[test]
    fn a_latched_terminal_stops_being_a_routing_target() {
        let (printer, _screen) = Printer::for_test_live_terminal(24, 100);
        let writer = LiveTracingWriter::new();
        writer.attach(&printer);
        assert!(
            writer.target().is_some(),
            "a live region is a routing target before anything fails"
        );

        printer
            .renderer
            .live
            .record_route_failure(&io::Error::from(io::ErrorKind::BrokenPipe));

        assert!(
            writer.target().is_none(),
            "a latched terminal is not a routing target"
        );
        assert!(
            writer.make_writer().route.is_none(),
            "each sink made after the latch falls through to stderr"
        );
    }

    /// The daemon's log line, composed the way the binary composes it:
    /// `HH:MM:SS  INFO <subsystem>: <sentence>`. The two spaces are not a
    /// choice — `tracing_subscriber`'s level token carries its own leading
    /// space, so a `%H:%M:%S` timer yields them and nothing formats the level
    /// by hand. A subscriber built without the timer prints `<sentence>` with
    /// no way to tell when the daemon said it, which is what shipped before.
    #[test]
    fn a_daemon_log_line_opens_with_its_local_time_and_level() {
        #[derive(Clone)]
        struct Capture(std::sync::Arc<std::sync::Mutex<String>>);
        impl io::Write for Capture {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.0
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push_str(&String::from_utf8_lossy(buf));
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        impl MakeWriter<'_> for Capture {
            type Writer = Self;
            fn make_writer(&self) -> Self::Writer {
                self.clone()
            }
        }

        let buf = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let subscriber = tracing_subscriber::fmt()
            // unfolded-writer-ok: a test capture read back as a String, not a stream anyone is looking at
            .with_writer(Capture(buf.clone()))
            .with_timer(LocalTimeOfDay)
            .with_ansi(false)
            .with_target(false)
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("reconcile: complete — nothing to do");
        });

        // raw-capture-ok: a tracing writer's buffer, asserted byte-exact — an ANSI-stripping read would pass with `with_ansi` dropped
        let line = buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let line = line.trim_end();
        let (stamp, rest) = line.split_at(8);
        assert!(
            stamp.len() == 8
                && stamp.as_bytes()[2] == b':'
                && stamp.as_bytes()[5] == b':'
                && stamp
                    .chars()
                    .filter(|c| *c != ':')
                    .all(|c| c.is_ascii_digit()),
            "a daemon log line opens with a local `HH:MM:SS` stamp: {line:?}"
        );
        assert_eq!(
            rest, "  INFO reconcile: complete — nothing to do",
            "the stamp is followed by exactly two spaces, the level, and the \
             `<subsystem>: <sentence>` message: {line:?}"
        );
    }
}
