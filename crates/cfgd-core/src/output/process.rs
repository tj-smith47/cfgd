use indicatif::{ProgressBar, ProgressStyle};

use std::collections::VecDeque;
use std::io::BufRead;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::{CommandOutput, Printer, Verbosity};

/// A line captured from a child process's stdout or stderr.
enum CapturedLine {
    Stdout(String),
    Stderr(String),
}

impl Printer {
    /// Run a command with live output display.
    ///
    /// TTY mode: shows a spinner with the last N lines of output in a bounded
    /// region. On success, collapses to a summary line. On failure, shows full
    /// stderr.
    ///
    /// Non-TTY / quiet mode: streams output lines as they arrive. Captures
    /// stdout/stderr for the return value.
    pub fn run_with_output(
        &self,
        cmd: &mut std::process::Command,
        label: &str,
    ) -> std::io::Result<CommandOutput> {
        let start = Instant::now();
        cmd.stdin(std::process::Stdio::null());

        if self.term.is_term() && self.verbosity != Verbosity::Quiet {
            self.run_with_progress(cmd, label, start)
        } else {
            self.run_streaming(cmd, label, start)
        }
    }

    /// Spawn background threads to read stdout/stderr from a child process,
    /// sending lines through the returned channel. The original sender is dropped
    /// so the receiver disconnects once both reader threads finish.
    fn spawn_output_readers(child: &mut std::process::Child) -> mpsc::Receiver<CapturedLine> {
        let (tx, rx) = mpsc::channel();

        if let Some(stdout) = child.stdout.take() {
            let tx = tx.clone();
            std::thread::spawn(move || {
                for line in std::io::BufReader::new(stdout)
                    .lines()
                    .map_while(Result::ok)
                {
                    let _ = tx.send(CapturedLine::Stdout(line));
                }
            });
        }
        if let Some(stderr) = child.stderr.take() {
            let tx = tx.clone();
            std::thread::spawn(move || {
                for line in std::io::BufReader::new(stderr)
                    .lines()
                    .map_while(Result::ok)
                {
                    let _ = tx.send(CapturedLine::Stderr(line));
                }
            });
        }
        drop(tx);
        rx
    }

    /// TTY path: bounded scrolling output region with spinner.
    fn run_with_progress(
        &self,
        cmd: &mut std::process::Command,
        label: &str,
        start: Instant,
    ) -> std::io::Result<CommandOutput> {
        const VISIBLE_LINES: usize = 5;

        let mut child = cmd
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;

        let pb = self.multi_progress.add(ProgressBar::new_spinner());

        // Build themed spinner frames so the animation respects the active theme
        let frames_raw = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let styled_frames: Vec<String> = frames_raw
            .iter()
            .map(|f| self.theme.info.apply_to(f).to_string())
            .collect();
        let mut tick_refs: Vec<&str> = styled_frames.iter().map(|s| s.as_str()).collect();
        tick_refs.push(" "); // final "done" frame (unused with finish_and_clear)
        pb.set_style(
            ProgressStyle::with_template("{spinner} {msg}")
                .unwrap_or_else(|_| ProgressStyle::default_spinner())
                .tick_strings(&tick_refs),
        );
        pb.set_message(label.to_string());
        pb.enable_steady_tick(Duration::from_millis(80));

        let rx = Self::spawn_output_readers(&mut child);

        let mut ring: VecDeque<String> = VecDeque::with_capacity(VISIBLE_LINES);
        let mut all_stdout = Vec::new();
        let mut all_stderr = Vec::new();

        loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(line) => {
                    let text = match &line {
                        CapturedLine::Stdout(s) => {
                            all_stdout.push(s.clone());
                            s
                        }
                        CapturedLine::Stderr(s) => {
                            all_stderr.push(s.clone());
                            s
                        }
                    };
                    if ring.len() >= VISIBLE_LINES {
                        ring.pop_front();
                    }
                    ring.push_back(text.clone());

                    let mut msg = label.to_string();
                    for l in &ring {
                        let display = if l.len() > 120 {
                            match l.get(..120) {
                                Some(s) => s,
                                None => l, // multi-byte boundary; show full line
                            }
                        } else {
                            l
                        };
                        msg.push_str(&format!("\n  {}", self.theme.muted.apply_to(display)));
                    }
                    pb.set_message(msg);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        let status = child.wait()?;
        let duration = start.elapsed();
        pb.finish_and_clear();

        if status.success() {
            self.success(&format!("{} ({}s)", label, duration.as_secs()));
        } else {
            self.error(&format!("{} — failed ({}s)", label, duration.as_secs()));
            for line in &all_stderr {
                let _ = self
                    .term
                    .write_line(&format!("  {}", self.theme.muted.apply_to(line)));
            }
        }

        Ok(CommandOutput {
            status,
            stdout: all_stdout.join("\n"),
            stderr: all_stderr.join("\n"),
            duration,
        })
    }

    /// Non-TTY / quiet path: stream output lines as they arrive.
    fn run_streaming(
        &self,
        cmd: &mut std::process::Command,
        label: &str,
        start: Instant,
    ) -> std::io::Result<CommandOutput> {
        let mut child = cmd
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;

        if self.verbosity != Verbosity::Quiet {
            self.info(label);
        }

        let rx = Self::spawn_output_readers(&mut child);

        let mut all_stdout = Vec::new();
        let mut all_stderr = Vec::new();

        for line in rx {
            match &line {
                CapturedLine::Stdout(s) => {
                    if self.verbosity != Verbosity::Quiet {
                        let _ = self.term.write_line(s);
                    }
                    all_stdout.push(s.clone());
                }
                CapturedLine::Stderr(s) => {
                    if self.verbosity != Verbosity::Quiet {
                        let _ = self.term.write_line(s);
                    }
                    all_stderr.push(s.clone());
                }
            }
        }

        let status = child.wait()?;
        let duration = start.elapsed();

        if status.success() {
            if self.verbosity != Verbosity::Quiet {
                self.success(&format!("{} ({}s)", label, duration.as_secs()));
            }
        } else {
            self.error(&format!("{} — failed ({}s)", label, duration.as_secs()));
        }

        Ok(CommandOutput {
            status,
            stdout: all_stdout.join("\n"),
            stderr: all_stderr.join("\n"),
            duration,
        })
    }
}
