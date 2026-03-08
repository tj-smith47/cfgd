// Centralized output system — all terminal interaction goes through Printer
// Methods defined in Phase 1 for use in later phases (4-9)
#![allow(dead_code)]

use console::{Style, Term};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use similar::{ChangeTag, TextDiff};
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use syntect::util::as_24_bit_terminal_escaped;

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

    pub icon_success: &'static str,
    pub icon_warning: &'static str,
    pub icon_error: &'static str,
    pub icon_info: &'static str,
    pub icon_pending: &'static str,
    pub icon_arrow: &'static str,
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

            icon_success: "✓",
            icon_warning: "⚠",
            icon_error: "✗",
            icon_info: "●",
            icon_pending: "○",
            icon_arrow: "→",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verbosity {
    Quiet,
    Normal,
    Verbose,
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
        Self {
            theme: Theme::default(),
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
        let icon = self.theme.success.apply_to(self.theme.icon_success);
        let _ = self.term.write_line(&format!("{} {}", icon, text));
    }

    pub fn warning(&self, text: &str) {
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        let icon = self.theme.warning.apply_to(self.theme.icon_warning);
        let _ = self.term.write_line(&format!("{} {}", icon, text));
    }

    pub fn error(&self, text: &str) {
        let icon = self.theme.error.apply_to(self.theme.icon_error);
        let _ = self.term.write_line(&format!("{} {}", icon, text));
    }

    pub fn info(&self, text: &str) {
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        let icon = self.theme.info.apply_to(self.theme.icon_info);
        let _ = self.term.write_line(&format!("{} {}", icon, text));
    }

    pub fn key_value(&self, key: &str, value: &str) {
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        let k = self.theme.key.apply_to(format!("{:>16}", key));
        let arrow = self.theme.muted.apply_to(self.theme.icon_arrow);
        let _ = self.term.write_line(&format!("{} {} {}", k, arrow, value));
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
                let icon = self.theme.info.apply_to(self.theme.icon_pending);
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

    pub fn prompt_select<'a>(
        &self,
        message: &str,
        options: &'a [String],
    ) -> std::result::Result<&'a String, inquire::InquireError> {
        let idx = inquire::Select::new(message, options.to_vec()).prompt()?;
        // Find the index of the selected item
        Ok(options.iter().find(|o| **o == idx).unwrap_or(&options[0]))
    }

    pub fn newline(&self) {
        let _ = self.term.write_line("");
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
        // In quiet mode, non-error output methods should not panic
        let printer = Printer::new(Verbosity::Quiet);
        printer.header("test");
        printer.success("test");
        printer.warning("test");
        printer.info("test");
        printer.key_value("key", "value");
    }

    #[test]
    fn printer_error_always_prints() {
        // Error should work in all verbosity modes
        let printer = Printer::new(Verbosity::Quiet);
        printer.error("this is an error");
    }

    #[test]
    fn verbosity_returns_correct_level() {
        let printer = Printer::new(Verbosity::Verbose);
        assert_eq!(printer.verbosity(), Verbosity::Verbose);
    }
}
