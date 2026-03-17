// Centralized output system — all terminal interaction goes through Printer

use console::{Color, Style, Term};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use similar::{ChangeTag, TextDiff};
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use syntect::util::as_24_bit_terminal_escaped;

use std::collections::VecDeque;
use std::io::BufRead;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::config::ThemeConfig;

// Default icons shared across all non-minimal presets
const ICON_SUCCESS: &str = "✓";
const ICON_WARNING: &str = "⚠";
const ICON_ERROR: &str = "✗";
const ICON_INFO: &str = "⚙";
const ICON_PENDING: &str = "○";
const ICON_ARROW: &str = "→";

pub struct Theme {
    pub success: Style,
    pub warning: Style,
    pub error: Style,
    pub info: Style,
    pub muted: Style,

    pub header: Style,
    pub subheader: Style,
    pub key: Style,
    pub value: Style,

    pub diff_add: Style,
    pub diff_remove: Style,
    pub diff_context: Style,

    pub icon_success: String,
    pub icon_warning: String,
    pub icon_error: String,
    pub icon_info: String,
    pub icon_pending: String,
    pub icon_arrow: String,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            success: Style::new().green(),
            warning: Style::new().yellow(),
            error: Style::new().red().bold(),
            info: Style::new().cyan(),
            muted: Style::new().dim(),

            header: Style::new().bold().cyan(),
            subheader: Style::new().bold(),
            key: Style::new().bold(),
            value: Style::new(),

            diff_add: Style::new().green(),
            diff_remove: Style::new().red(),
            diff_context: Style::new().dim(),

            icon_success: ICON_SUCCESS.into(),
            icon_warning: ICON_WARNING.into(),
            icon_error: ICON_ERROR.into(),
            icon_info: ICON_INFO.into(),
            icon_pending: ICON_PENDING.into(),
            icon_arrow: ICON_ARROW.into(),
        }
    }
}

impl Theme {
    pub fn from_config(config: Option<&ThemeConfig>) -> Self {
        let config = match config {
            Some(c) => c,
            None => return Self::default(),
        };

        let mut theme = Self::from_preset(&config.name);

        // Apply hex color overrides
        let ov = &config.overrides;
        if let Some(ref c) = ov.success {
            apply_color(&mut theme.success, c);
        }
        if let Some(ref c) = ov.warning {
            apply_color(&mut theme.warning, c);
        }
        if let Some(ref c) = ov.error {
            apply_color(&mut theme.error, c);
        }
        if let Some(ref c) = ov.info {
            apply_color(&mut theme.info, c);
        }
        if let Some(ref c) = ov.muted {
            apply_color(&mut theme.muted, c);
        }
        if let Some(ref c) = ov.header {
            apply_color(&mut theme.header, c);
        }
        if let Some(ref c) = ov.subheader {
            apply_color(&mut theme.subheader, c);
        }
        if let Some(ref c) = ov.key {
            apply_color(&mut theme.key, c);
        }
        if let Some(ref c) = ov.value {
            apply_color(&mut theme.value, c);
        }
        if let Some(ref c) = ov.diff_add {
            apply_color(&mut theme.diff_add, c);
        }
        if let Some(ref c) = ov.diff_remove {
            apply_color(&mut theme.diff_remove, c);
        }
        if let Some(ref c) = ov.diff_context {
            apply_color(&mut theme.diff_context, c);
        }

        // Apply icon overrides
        if let Some(ref v) = ov.icon_success {
            theme.icon_success = v.clone();
        }
        if let Some(ref v) = ov.icon_warning {
            theme.icon_warning = v.clone();
        }
        if let Some(ref v) = ov.icon_error {
            theme.icon_error = v.clone();
        }
        if let Some(ref v) = ov.icon_info {
            theme.icon_info = v.clone();
        }
        if let Some(ref v) = ov.icon_pending {
            theme.icon_pending = v.clone();
        }
        if let Some(ref v) = ov.icon_arrow {
            theme.icon_arrow = v.clone();
        }

        theme
    }

    pub fn from_preset(name: &str) -> Self {
        match name {
            "dracula" => Self::dracula(),
            "solarized-dark" => Self::solarized_dark(),
            "solarized-light" => Self::solarized_light(),
            "minimal" => Self::minimal(),
            _ => Self::default(),
        }
    }

    fn dracula() -> Self {
        Self {
            success: style_from_hex("#50fa7b"),
            warning: style_from_hex("#f1fa8c"),
            error: style_from_hex("#ff5555").bold(),
            info: style_from_hex("#8be9fd"),
            muted: style_from_hex("#6272a4"),

            header: style_from_hex("#bd93f9").bold(),
            subheader: Style::new().bold(),
            key: style_from_hex("#ff79c6").bold(),
            value: style_from_hex("#f8f8f2"),

            diff_add: style_from_hex("#50fa7b"),
            diff_remove: style_from_hex("#ff5555"),
            diff_context: style_from_hex("#6272a4"),

            icon_success: ICON_SUCCESS.into(),
            icon_warning: ICON_WARNING.into(),
            icon_error: ICON_ERROR.into(),
            icon_info: ICON_INFO.into(),
            icon_pending: ICON_PENDING.into(),
            icon_arrow: ICON_ARROW.into(),
        }
    }

    fn solarized_dark() -> Self {
        Self {
            success: style_from_hex("#859900"),
            warning: style_from_hex("#b58900"),
            error: style_from_hex("#dc322f").bold(),
            info: style_from_hex("#268bd2"),
            muted: style_from_hex("#586e75"),

            header: style_from_hex("#268bd2").bold(),
            subheader: Style::new().bold(),
            key: style_from_hex("#2aa198").bold(),
            value: style_from_hex("#839496"),

            diff_add: style_from_hex("#859900"),
            diff_remove: style_from_hex("#dc322f"),
            diff_context: style_from_hex("#586e75"),

            icon_success: ICON_SUCCESS.into(),
            icon_warning: ICON_WARNING.into(),
            icon_error: ICON_ERROR.into(),
            icon_info: ICON_INFO.into(),
            icon_pending: ICON_PENDING.into(),
            icon_arrow: ICON_ARROW.into(),
        }
    }

    fn solarized_light() -> Self {
        Self {
            success: style_from_hex("#859900"),
            warning: style_from_hex("#b58900"),
            error: style_from_hex("#dc322f").bold(),
            info: style_from_hex("#268bd2"),
            muted: style_from_hex("#93a1a1"),

            header: style_from_hex("#268bd2").bold(),
            subheader: Style::new().bold(),
            key: style_from_hex("#2aa198").bold(),
            value: style_from_hex("#657b83"),

            diff_add: style_from_hex("#859900"),
            diff_remove: style_from_hex("#dc322f"),
            diff_context: style_from_hex("#93a1a1"),

            icon_success: ICON_SUCCESS.into(),
            icon_warning: ICON_WARNING.into(),
            icon_error: ICON_ERROR.into(),
            icon_info: ICON_INFO.into(),
            icon_pending: ICON_PENDING.into(),
            icon_arrow: ICON_ARROW.into(),
        }
    }

    fn minimal() -> Self {
        Self {
            success: Style::new(),
            warning: Style::new(),
            error: Style::new().bold(),
            info: Style::new(),
            muted: Style::new().dim(),

            header: Style::new().bold(),
            subheader: Style::new().bold(),
            key: Style::new().bold(),
            value: Style::new(),

            diff_add: Style::new(),
            diff_remove: Style::new(),
            diff_context: Style::new().dim(),

            icon_success: "+".into(),
            icon_warning: "!".into(),
            icon_error: "x".into(),
            icon_info: "-".into(),
            icon_pending: " ".into(),
            icon_arrow: ">".into(),
        }
    }
}

/// Parse a hex color string (#rrggbb or rrggbb) into a console::Color.
fn parse_hex_color(hex: &str) -> Option<Color> {
    let hex = hex.strip_prefix('#').unwrap_or(hex);
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Color::Color256(ansi256_from_rgb(r, g, b)))
}

/// Map RGB to the nearest ANSI 256-color index.
fn ansi256_from_rgb(r: u8, g: u8, b: u8) -> u8 {
    // Check grayscale ramp (232-255) first
    if r == g && g == b {
        if r < 8 {
            return 16;
        }
        if r > 248 {
            return 231;
        }
        return (((r as u16 - 8) * 24 / 247) as u8) + 232;
    }
    // Map to 6x6x6 color cube (indices 16-231)
    let ri = (r as u16 * 5 / 255) as u8;
    let gi = (g as u16 * 5 / 255) as u8;
    let bi = (b as u16 * 5 / 255) as u8;
    16 + 36 * ri + 6 * gi + bi
}

fn style_from_hex(hex: &str) -> Style {
    match parse_hex_color(hex) {
        Some(color) => Style::new().fg(color),
        None => Style::new(),
    }
}

fn apply_color(style: &mut Style, hex: &str) {
    if let Some(color) = parse_hex_color(hex) {
        *style = Style::new().fg(color);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verbosity {
    Quiet,
    Normal,
    Verbose,
}

/// A line captured from a child process's stdout or stderr.
enum CapturedLine {
    Stdout(String),
    Stderr(String),
}

/// Result of running a command with live output display.
pub struct CommandOutput {
    pub status: std::process::ExitStatus,
    pub stdout: String,
    pub stderr: String,
    pub duration: Duration,
}

pub struct Printer {
    theme: Theme,
    term: Term,
    multi_progress: MultiProgress,
    syntax_set: SyntaxSet,
    theme_set: ThemeSet,
    verbosity: Verbosity,
}

impl Printer {
    pub fn new(verbosity: Verbosity) -> Self {
        Self::with_theme(verbosity, None)
    }

    pub fn with_theme(verbosity: Verbosity, theme_config: Option<&ThemeConfig>) -> Self {
        Self {
            theme: Theme::from_config(theme_config),
            term: Term::stderr(),
            multi_progress: MultiProgress::new(),
            syntax_set: SyntaxSet::load_defaults_newlines(),
            theme_set: ThemeSet::load_defaults(),
            verbosity,
        }
    }

    pub fn verbosity(&self) -> Verbosity {
        self.verbosity
    }

    pub fn header(&self, text: &str) {
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        let styled = self.theme.header.apply_to(format!("=== {} ===", text));
        let _ = self.term.write_line(&styled.to_string());
    }

    pub fn subheader(&self, text: &str) {
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        let styled = self.theme.subheader.apply_to(text);
        let _ = self.term.write_line(&styled.to_string());
    }

    pub fn success(&self, text: &str) {
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        let icon = self.theme.success.apply_to(&self.theme.icon_success);
        let _ = self.term.write_line(&format!("{} {}", icon, text));
    }

    pub fn warning(&self, text: &str) {
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        let icon = self.theme.warning.apply_to(&self.theme.icon_warning);
        let _ = self.term.write_line(&format!("{} {}", icon, text));
    }

    pub fn error(&self, text: &str) {
        let icon = self.theme.error.apply_to(&self.theme.icon_error);
        let _ = self.term.write_line(&format!("{} {}", icon, text));
    }

    pub fn info(&self, text: &str) {
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        let icon = self.theme.info.apply_to(&self.theme.icon_info);
        let _ = self.term.write_line(&format!("{} {}", icon, text));
    }

    pub fn key_value(&self, key: &str, value: &str) {
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        let k = self.theme.key.apply_to(format!("{:>16}", key));
        let arrow = self.theme.muted.apply_to(&self.theme.icon_arrow);
        let v = self.theme.value.apply_to(value);
        let _ = self.term.write_line(&format!("{} {} {}", k, arrow, v));
    }

    pub fn diff(&self, old: &str, new: &str) {
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        let diff = TextDiff::from_lines(old, new);
        for change in diff.iter_all_changes() {
            let (sign, style) = match change.tag() {
                ChangeTag::Delete => ("-", &self.theme.diff_remove),
                ChangeTag::Insert => ("+", &self.theme.diff_add),
                ChangeTag::Equal => (" ", &self.theme.diff_context),
            };
            let line = style.apply_to(format!("{}{}", sign, change));
            let _ = self.term.write_str(&line.to_string());
        }
    }

    pub fn syntax_highlight(&self, code: &str, language: &str) {
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        let syntax = self
            .syntax_set
            .find_syntax_by_token(language)
            .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text());
        let theme = &self.theme_set.themes["base16-ocean.dark"];
        let mut highlighter = HighlightLines::new(syntax, theme);

        for line in code.lines() {
            match highlighter.highlight_line(line, &self.syntax_set) {
                Ok(ranges) => {
                    let escaped = as_24_bit_terminal_escaped(&ranges[..], false);
                    let _ = self.term.write_line(&format!("{}\x1b[0m", escaped));
                }
                Err(_) => {
                    let _ = self.term.write_line(line);
                }
            }
        }
    }

    pub fn progress_bar(&self, total: u64, message: &str) -> ProgressBar {
        let pb = self.multi_progress.add(ProgressBar::new(total));
        pb.set_style(
            ProgressStyle::with_template("{spinner:.cyan} [{bar:30.cyan/dim}] {pos}/{len} {msg}")
                .unwrap_or_else(|_| ProgressStyle::default_bar())
                .progress_chars("━╸─"),
        );
        pb.set_message(message.to_string());
        pb
    }

    pub fn multi_progress(&self) -> &MultiProgress {
        &self.multi_progress
    }

    pub fn plan_phase(&self, name: &str, items: &[String]) {
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        let header = self.theme.subheader.apply_to(format!("Phase: {}", name));
        let _ = self.term.write_line(&header.to_string());
        if items.is_empty() {
            let muted = self.theme.muted.apply_to("  (nothing to do)");
            let _ = self.term.write_line(&muted.to_string());
        } else {
            for item in items {
                let icon = self.theme.info.apply_to(&self.theme.icon_pending);
                let _ = self.term.write_line(&format!("  {} {}", icon, item));
            }
        }
    }

    pub fn table(&self, headers: &[&str], rows: &[Vec<String>]) {
        if self.verbosity == Verbosity::Quiet {
            return;
        }

        let col_count = headers.len();
        let mut widths = vec![0usize; col_count];
        for (i, h) in headers.iter().enumerate() {
            widths[i] = h.len();
        }
        for row in rows {
            for (i, cell) in row.iter().enumerate().take(col_count) {
                widths[i] = widths[i].max(cell.len());
            }
        }

        // Header row
        let header_line: String = headers
            .iter()
            .enumerate()
            .map(|(i, h)| format!("{:<width$}", h, width = widths[i]))
            .collect::<Vec<_>>()
            .join("  ");
        let styled_header = self.theme.subheader.apply_to(&header_line);
        let _ = self.term.write_line(&styled_header.to_string());

        // Separator
        let sep: String = widths
            .iter()
            .map(|w| "─".repeat(*w))
            .collect::<Vec<_>>()
            .join("──");
        let styled_sep = self.theme.muted.apply_to(&sep);
        let _ = self.term.write_line(&styled_sep.to_string());

        // Data rows
        for row in rows {
            let line: String = row
                .iter()
                .enumerate()
                .take(col_count)
                .map(|(i, cell)| format!("{:<width$}", cell, width = widths[i]))
                .collect::<Vec<_>>()
                .join("  ");
            let _ = self.term.write_line(&line);
        }
    }

    pub fn prompt_confirm(
        &self,
        message: &str,
    ) -> std::result::Result<bool, inquire::InquireError> {
        inquire::Confirm::new(message).with_default(false).prompt()
    }

    pub fn prompt_confirm_with_default(
        &self,
        message: &str,
        default: bool,
    ) -> std::result::Result<bool, inquire::InquireError> {
        inquire::Confirm::new(message)
            .with_default(default)
            .prompt()
    }

    pub fn prompt_select<'a>(
        &self,
        message: &str,
        options: &'a [String],
    ) -> std::result::Result<&'a String, inquire::InquireError> {
        let idx = inquire::Select::new(message, options.to_vec()).prompt()?;
        Ok(options.iter().find(|o| **o == idx).unwrap_or(&options[0]))
    }

    pub fn prompt_text(
        &self,
        message: &str,
        default: &str,
    ) -> std::result::Result<String, inquire::InquireError> {
        inquire::Text::new(message).with_default(default).prompt()
    }

    pub fn newline(&self) {
        let _ = self.term.write_line("");
    }

    /// Write a line to stdout (for machine-readable data output, not UI).
    /// Used by commands like `config get` whose output may be captured by scripts.
    pub fn stdout_line(&self, text: &str) {
        let stdout = Term::stdout();
        let _ = stdout.write_line(text);
    }

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
    fn spawn_output_readers(
        child: &mut std::process::Child,
    ) -> mpsc::Receiver<CapturedLine> {
        let (tx, rx) = mpsc::channel();

        if let Some(stdout) = child.stdout.take() {
            let tx = tx.clone();
            std::thread::spawn(move || {
                for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
                    let _ = tx.send(CapturedLine::Stdout(line));
                }
            });
        }
        if let Some(stderr) = child.stderr.take() {
            let tx = tx.clone();
            std::thread::spawn(move || {
                for line in std::io::BufReader::new(stderr).lines().map_while(Result::ok) {
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
                        let display = if l.len() > 120 { &l[..120] } else { l };
                        msg.push_str(&format!(
                            "\n  {}",
                            self.theme.muted.apply_to(display)
                        ));
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
                let _ = self.term.write_line(&format!(
                    "  {}",
                    self.theme.muted.apply_to(line)
                ));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_theme_has_icons() {
        let theme = Theme::default();
        assert!(!theme.icon_success.is_empty());
        assert!(!theme.icon_warning.is_empty());
        assert!(!theme.icon_error.is_empty());
    }

    #[test]
    fn printer_respects_quiet_verbosity() {
        let printer = Printer::new(Verbosity::Quiet);
        printer.header("test");
        printer.success("test");
        printer.warning("test");
        printer.info("test");
        printer.key_value("key", "value");
    }

    #[test]
    fn printer_error_always_prints() {
        let printer = Printer::new(Verbosity::Quiet);
        printer.error("this is an error");
    }

    #[test]
    fn verbosity_returns_correct_level() {
        let printer = Printer::new(Verbosity::Verbose);
        assert_eq!(printer.verbosity(), Verbosity::Verbose);
    }

    #[test]
    fn parse_hex_valid() {
        let color = parse_hex_color("#ff5555");
        assert!(color.is_some());
    }

    #[test]
    fn parse_hex_no_hash() {
        let color = parse_hex_color("50fa7b");
        assert!(color.is_some());
    }

    #[test]
    fn parse_hex_invalid() {
        assert!(parse_hex_color("xyz").is_none());
        assert!(parse_hex_color("#gg0000").is_none());
        assert!(parse_hex_color("#ff").is_none());
    }

    #[test]
    fn named_presets_exist() {
        let _ = Theme::from_preset("default");
        let _ = Theme::from_preset("dracula");
        let _ = Theme::from_preset("solarized-dark");
        let _ = Theme::from_preset("solarized-light");
        let _ = Theme::from_preset("minimal");
    }

    #[test]
    fn unknown_preset_falls_back_to_default() {
        let theme = Theme::from_preset("nonexistent");
        assert_eq!(theme.icon_success, "✓");
    }

    #[test]
    fn from_config_with_overrides() {
        let config = ThemeConfig {
            name: "default".into(),
            overrides: crate::config::ThemeOverrides {
                icon_success: Some("OK".into()),
                success: Some("#50fa7b".into()),
                ..Default::default()
            },
        };
        let theme = Theme::from_config(Some(&config));
        assert_eq!(theme.icon_success, "OK");
    }

    #[test]
    fn from_config_none_gives_default() {
        let theme = Theme::from_config(None);
        assert_eq!(theme.icon_success, "✓");
    }

    #[test]
    fn minimal_theme_uses_ascii_icons() {
        let theme = Theme::from_preset("minimal");
        assert_eq!(theme.icon_success, "+");
        assert_eq!(theme.icon_error, "x");
        assert_eq!(theme.icon_arrow, ">");
    }

    #[test]
    fn run_with_output_captures_stdout() {
        let printer = Printer::new(Verbosity::Quiet);
        let output = printer
            .run_with_output(
                &mut std::process::Command::new("echo").arg("hello"),
                "test echo",
            )
            .unwrap();
        assert!(output.status.success());
        assert!(output.stdout.contains("hello"));
    }

    #[test]
    fn run_with_output_captures_stderr() {
        let printer = Printer::new(Verbosity::Quiet);
        let output = printer
            .run_with_output(
                std::process::Command::new("sh")
                    .arg("-c")
                    .arg("echo error >&2"),
                "test stderr",
            )
            .unwrap();
        assert!(output.status.success());
        assert!(output.stderr.contains("error"));
    }

    #[test]
    fn run_with_output_reports_failure() {
        let printer = Printer::new(Verbosity::Quiet);
        let output = printer
            .run_with_output(&mut std::process::Command::new("false"), "test failure")
            .unwrap();
        assert!(!output.status.success());
    }

    #[test]
    fn run_with_output_tracks_duration() {
        let printer = Printer::new(Verbosity::Quiet);
        let output = printer
            .run_with_output(&mut std::process::Command::new("true"), "test duration")
            .unwrap();
        assert!(output.duration.as_secs() < 5);
    }

    #[test]
    fn run_with_output_spawn_error() {
        let printer = Printer::new(Verbosity::Quiet);
        let result = printer.run_with_output(
            &mut std::process::Command::new("/nonexistent/binary"),
            "test spawn error",
        );
        assert!(result.is_err());
    }
}
