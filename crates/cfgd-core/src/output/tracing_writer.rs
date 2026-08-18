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
    bars: Arc<RwLock<Option<indicatif::MultiProgress>>>,
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
        let mut slot = self.bars.write().unwrap_or_else(|e| e.into_inner());
        *slot = Some(printer.multi_progress.clone());
    }

    fn target(&self) -> Option<indicatif::MultiProgress> {
        self.bars
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .filter(|mp| !mp.is_hidden())
            .cloned()
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LiveTracingWriter {
    type Writer = LiveTracingSink;

    fn make_writer(&'a self) -> Self::Writer {
        LiveTracingSink {
            bars: self.target(),
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
    bars: Option<indicatif::MultiProgress>,
    buf: Vec<u8>,
}

impl LiveTracingSink {
    fn emit(&mut self) {
        if self.buf.is_empty() {
            return;
        }
        let bytes = std::mem::take(&mut self.buf);
        if let Some(mp) = &self.bars {
            let text = String::from_utf8_lossy(&bytes);
            // `println` supplies the line ending itself, and a trailing one
            // here would draw a blank row into the region on every event.
            if mp.println(text.trim_end_matches(['\r', '\n'])).is_ok() {
                return;
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

    /// An unattached writer is the plain stderr writer, so installing the
    /// subscriber before the printer exists loses no event.
    #[test]
    fn an_unattached_writer_still_emits() {
        let writer = LiveTracingWriter::new();
        assert!(
            writer.target().is_none(),
            "a fresh writer must have no region to route through"
        );
        writer
            .make_writer()
            .write_all(b"WARN before the printer existed\n")
            .expect("the sink buffers, so a write cannot fail");
    }
}
