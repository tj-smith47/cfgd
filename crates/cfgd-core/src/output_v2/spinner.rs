//! `Spinner` and `ProgressBar` — live progress indicators.
//!
//! `Spinner::finish_ok` / `finish_warn` / `finish_fail` / `finish_skipped`
//! return a `StatusBuilder` so the caller can chain `.detail` / `.duration`
//! / `.target` before the Status commits on Drop.
//!
//! A `Spinner` dropped without an explicit finish emits a `Status(Info)` so
//! the spinner doesn't disappear silently — abandonment leaves a record.
use std::io::IsTerminal;
use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

use indicatif::{ProgressBar as IndProgressBar, ProgressStyle};

use super::Role;
use super::renderer::{Renderer, Writer};
use super::status_builder::StatusBuilder;

pub(crate) fn stderr_is_terminal() -> bool {
    std::io::stderr().is_terminal()
}

/// Live spinner. Drop without `finish_*()` emits a `Status(Info)` with the
/// spinner message at the active depth — leaves a record so the spinner
/// doesn't disappear silently.
pub struct Spinner<'p> {
    pub(crate) renderer: Arc<Renderer>,
    pub(crate) sink: Arc<dyn Writer>,
    pub(crate) depth: usize,
    pub(crate) bar: IndProgressBar,
    pub(crate) message: String,
    pub(crate) finished: bool,
    pub(crate) _phantom: PhantomData<&'p ()>,
}

impl<'p> Spinner<'p> {
    pub fn set_message(&self, text: impl Into<String>) {
        self.bar.set_message(text.into());
    }

    pub fn finish_ok(self, final_text: impl Into<String>) -> StatusBuilder<'p> {
        self.finish_with(Role::Ok, final_text)
    }
    pub fn finish_warn(self, final_text: impl Into<String>) -> StatusBuilder<'p> {
        self.finish_with(Role::Warn, final_text)
    }
    pub fn finish_fail(self, final_text: impl Into<String>) -> StatusBuilder<'p> {
        self.finish_with(Role::Fail, final_text)
    }
    pub fn finish_skipped(self, final_text: impl Into<String>) -> StatusBuilder<'p> {
        self.finish_with(Role::Skipped, final_text)
    }

    fn finish_with(mut self, role: Role, subject: impl Into<String>) -> StatusBuilder<'p> {
        self.bar.finish_and_clear();
        self.finished = true;
        // The Arc clones below give the returned StatusBuilder an
        // independent reference to the renderer and sink. `self` is moved
        // into this fn and dropped at the end of the call, but the
        // StatusBuilder must outlive it (Drop fires when the caller drops
        // the builder).
        StatusBuilder::new(
            self.renderer.clone(),
            self.sink.clone(),
            self.depth,
            role,
            subject,
        )
    }
}

impl Drop for Spinner<'_> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        self.bar.finish_and_clear();
        // Emit an Info Status so the spinner leaves a record.
        //
        // The `self.renderer.clone()` and `self.sink.clone()` Arc-clones
        // inside `StatusBuilder::new` (passed as arguments below) are
        // LOAD-BEARING. The StatusBuilder needs an independent Arc so that
        // when `self` finishes dropping and its Arc fields are released,
        // the builder (whose own Drop fires at the end of this function via
        // the `drop(sb)` call) still holds a live reference to the
        // renderer and sink.
        let msg = std::mem::take(&mut self.message);
        let sb = StatusBuilder::new(
            self.renderer.clone(),
            self.sink.clone(),
            self.depth,
            Role::Info,
            msg,
        );
        drop(sb);
    }
}

/// Bounded progress bar.
pub struct ProgressBar<'p> {
    pub(crate) bar: IndProgressBar,
    pub(crate) _phantom: PhantomData<&'p ()>,
}

impl<'p> ProgressBar<'p> {
    pub fn inc(&self, delta: u64) {
        self.bar.inc(delta);
    }
    pub fn set_position(&self, pos: u64) {
        self.bar.set_position(pos);
    }
    pub fn set_message(&self, m: impl Into<String>) {
        self.bar.set_message(m.into());
    }
    pub fn finish(self) {
        self.bar.finish_and_clear();
    }
}

/// Build a styled spinner ProgressBar attached to a MultiProgress.
pub(crate) fn build_spinner(
    multi: &indicatif::MultiProgress,
    renderer: &Renderer,
    message: &str,
) -> IndProgressBar {
    let pb = multi.add(IndProgressBar::new_spinner());
    let frames_raw = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let styled: Vec<String> = frames_raw
        .iter()
        .map(|f| renderer.theme.info.apply_to(f).to_string())
        .collect();
    let mut tick_refs: Vec<&str> = styled.iter().map(|s| s.as_str()).collect();
    tick_refs.push(" ");
    pb.set_style(
        ProgressStyle::with_template("{spinner} {msg}")
            .unwrap_or_else(|_| ProgressStyle::default_spinner())
            .tick_strings(&tick_refs),
    );
    pb.set_message(message.to_string());
    pb.enable_steady_tick(Duration::from_millis(80));
    pb
}

pub(crate) fn build_progress_bar(
    multi: &indicatif::MultiProgress,
    total: u64,
    message: &str,
) -> IndProgressBar {
    let pb = multi.add(IndProgressBar::new(total));
    pb.set_style(
        ProgressStyle::with_template("{spinner:.cyan} [{bar:30.cyan/dim}] {pos}/{len} {msg}")
            .unwrap_or_else(|_| ProgressStyle::default_bar())
            .progress_chars("━╸─"),
    );
    pb.set_message(message.to_string());
    pb
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::super::renderer::{Renderer, StringSink};
    use super::super::{Theme, Verbosity};
    use super::*;

    fn renderer() -> Arc<Renderer> {
        Arc::new(Renderer::new(Theme::default(), Verbosity::Normal))
    }

    fn sink_for(buf: &Arc<Mutex<String>>) -> Arc<dyn Writer> {
        Arc::new(StringSink(buf.clone()))
    }

    fn strip_ansi(s: &str) -> String {
        // ANSI CSI sequences are all ASCII, so we can walk chars and skip
        // them without splitting multi-byte UTF-8 glyphs like ✓ ✗ —. T28
        // will consolidate this helper across the output_v2 test modules.
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' && chars.peek() == Some(&'[') {
                chars.next(); // consume '['
                for inner in chars.by_ref() {
                    if inner == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn finish_ok_emits_status_at_section_depth() {
        let r = renderer();
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = sink_for(&buf);
        // Hidden bar (no TTY in test); finish_ok still emits the Status line.
        let sp = Spinner {
            renderer: r.clone(),
            sink: sink.clone(),
            depth: 1,
            bar: indicatif::ProgressBar::hidden(),
            message: "doing work".into(),
            finished: false,
            _phantom: std::marker::PhantomData,
        };
        let _ = sp.finish_ok("done");
        // _ drops here → Status committed
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(out.contains("  ✓ done"), "got: {out:?}");
    }

    #[test]
    fn drop_without_finish_emits_info_record() {
        let r = renderer();
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = sink_for(&buf);
        {
            let _sp = Spinner {
                renderer: r.clone(),
                sink: sink.clone(),
                depth: 0,
                bar: indicatif::ProgressBar::hidden(),
                message: "abandoned".into(),
                finished: false,
                _phantom: std::marker::PhantomData,
            };
        }
        let out = strip_ansi(&buf.lock().unwrap());
        // Info role has no icon; subject text appears.
        assert!(out.contains("abandoned"), "got: {out:?}");
    }
}
