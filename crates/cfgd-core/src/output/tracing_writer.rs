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

use std::io::{self, Write};
use std::sync::{Arc, RwLock};

use super::Printer;
use super::renderer::LiveBarState;

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
        if self.buf.is_empty() {
            return;
        }
        let bytes = std::mem::take(&mut self.buf);
        if let Some(route) = &self.route {
            let text = String::from_utf8_lossy(&bytes);
            // `println` supplies the line ending itself, and a trailing one
            // here would draw a blank row into the region on every event.
            match route.bars.println(text.trim_end_matches(['\r', '\n'])) {
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
        let mut err = io::stderr().lock();
        let _ = err.write_all(&bytes);
        let _ = err.flush();
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
}
