use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

use std::time::Duration;

use super::printer::stderr_is_terminal;
use super::{Printer, Verbosity};

impl Printer {
    pub fn progress_bar(&self, total: u64, message: &str) -> ProgressBar {
        // Mirror spinner()'s gates: Quiet (including auto-Quiet under `-o json`)
        // or a non-TTY stderr must never emit animated progress frames.
        if self.verbosity == Verbosity::Quiet || !stderr_is_terminal() {
            return ProgressBar::hidden();
        }
        let pb = self.multi_progress.add(ProgressBar::new(total));
        pb.set_style(
            ProgressStyle::with_template("{spinner:.cyan} [{bar:30.cyan/dim}] {pos}/{len} {msg}")
                .unwrap_or_else(|_| ProgressStyle::default_bar())
                .progress_chars("━╸─"),
        );
        pb.set_message(message.to_string());
        pb
    }

    pub fn spinner(&self, message: &str) -> ProgressBar {
        if self.verbosity == Verbosity::Quiet || !stderr_is_terminal() {
            return ProgressBar::hidden();
        }
        let pb = self.multi_progress.add(ProgressBar::new_spinner());
        let frames_raw = [
            "\u{28fb}", "\u{28d9}", "\u{2839}", "\u{2838}", "\u{283c}", "\u{2834}", "\u{2826}",
            "\u{2827}", "\u{2807}", "\u{280f}",
        ];
        let styled_frames: Vec<String> = frames_raw
            .iter()
            .map(|f| self.theme.info.apply_to(f).to_string())
            .collect();
        let mut tick_refs: Vec<&str> = styled_frames.iter().map(|s| s.as_str()).collect();
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

    pub fn multi_progress(&self) -> &MultiProgress {
        &self.multi_progress
    }
}
