use console::Term;
use indicatif::MultiProgress;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;

use std::io::IsTerminal;
use std::sync::{Arc, Mutex};

use super::theme::Theme;
use super::{OutputFormat, Printer, Verbosity};
use crate::config::ThemeConfig;

/// Whether stderr is attached to a terminal. Gate animated output (spinners,
/// progress bars) on this so `cfgd apply 2>err.log` captures a clean log
/// without tick frames.
pub(super) fn stderr_is_terminal() -> bool {
    std::io::stderr().is_terminal()
}

/// Build an `InquireError::Custom` for the "structured-output asked for an
/// interactive prompt" case. `-o json|yaml|name|jsonpath|template` implies a
/// non-interactive consumer; hanging on `inquire` here would deadlock scripts.
pub(super) fn non_interactive_err(prompt: &str) -> inquire::InquireError {
    inquire::InquireError::Custom(
        format!(
            "refusing to prompt for '{prompt}' in non-interactive/structured output mode \
             (re-run without -o json or supply the answer via a flag / env var)"
        )
        .into(),
    )
}

impl Printer {
    pub fn new(verbosity: Verbosity) -> Self {
        Self::with_theme(verbosity, None)
    }

    pub fn with_theme(verbosity: Verbosity, theme_config: Option<&ThemeConfig>) -> Self {
        Self::with_format(verbosity, theme_config, OutputFormat::Table)
    }

    /// Disable color output globally. Wraps the `console` crate's color toggle
    /// so that no other module needs to depend on `console` directly.
    pub fn disable_colors() {
        console::set_colors_enabled(false);
        console::set_colors_enabled_stderr(false);
    }

    /// Routing: status output (`header`, `success`, `warning`, `error`,
    /// `info`, `key_value`, progress spinners) goes to stderr via
    /// `Term::stderr()` so pipelines like `cfgd status -o json | jq` keep
    /// the user-facing decoration off stdout. Structured output
    /// (`write_structured`, format renderers) writes to stdout. This
    /// split is hard-wired here — do not change it in call sites.
    ///
    /// Respects `NO_COLOR` (per https://no-color.org/) and `TERM=dumb`
    /// by disabling the `console` crate's styling before the Printer is
    /// constructed, so every Printer — including ones created by the
    /// daemon or tests — honors those environment variables without the
    /// caller having to re-check them.
    pub fn with_format(
        verbosity: Verbosity,
        theme_config: Option<&ThemeConfig>,
        output_format: OutputFormat,
    ) -> Self {
        if std::env::var_os("NO_COLOR").is_some()
            || std::env::var_os("TERM").is_some_and(|t| t == "dumb")
        {
            Self::disable_colors();
        }

        // Auto-quiet when structured output is active
        let verbosity = match &output_format {
            OutputFormat::Table | OutputFormat::Wide => verbosity,
            _ => Verbosity::Quiet,
        };
        Self {
            theme: Theme::from_config(theme_config),
            term: Term::stderr(),
            multi_progress: MultiProgress::new(),
            syntax_set: SyntaxSet::load_defaults_newlines(),
            theme_set: ThemeSet::load_defaults(),
            verbosity,
            output_format,
            test_buf: None,
        }
    }

    pub fn verbosity(&self) -> Verbosity {
        self.verbosity
    }

    /// Create a Printer that captures all output to a shared buffer.
    /// Use in tests to verify output content regardless of verbosity.
    pub fn for_test() -> (Self, Arc<Mutex<String>>) {
        let buf = Arc::new(Mutex::new(String::new()));
        let printer = Self {
            theme: Theme::from_config(None),
            term: Term::stderr(),
            multi_progress: MultiProgress::new(),
            syntax_set: SyntaxSet::load_defaults_newlines(),
            theme_set: ThemeSet::load_defaults(),
            verbosity: Verbosity::Quiet,
            output_format: OutputFormat::Table,
            test_buf: Some(buf.clone()),
        };
        (printer, buf)
    }

    /// Create a Printer with a specific output format that captures to a buffer.
    pub fn for_test_with_format(output_format: OutputFormat) -> (Self, Arc<Mutex<String>>) {
        let buf = Arc::new(Mutex::new(String::new()));
        let printer = Self {
            theme: Theme::from_config(None),
            term: Term::stderr(),
            multi_progress: MultiProgress::new(),
            syntax_set: SyntaxSet::load_defaults_newlines(),
            theme_set: ThemeSet::load_defaults(),
            verbosity: Verbosity::Quiet,
            output_format,
            test_buf: Some(buf.clone()),
        };
        (printer, buf)
    }

    /// Append a line to the test buffer (no-op when buffer is absent).
    pub(super) fn capture(&self, text: &str) {
        if let Some(ref buf) = self.test_buf {
            let mut b = buf.lock().unwrap_or_else(|e| e.into_inner());
            b.push_str(text);
            b.push('\n');
        }
    }

    pub fn header(&self, text: &str) {
        self.capture(&format!("=== {} ===", text));
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        let styled = self.theme.header.apply_to(format!("=== {} ===", text));
        let _ = self.term.write_line(&styled.to_string());
    }

    pub fn subheader(&self, text: &str) {
        self.capture(text);
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        let styled = self.theme.subheader.apply_to(text);
        let _ = self.term.write_line(&styled.to_string());
    }

    pub fn success(&self, text: &str) {
        self.capture(text);
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        let icon = self.theme.success.apply_to(&self.theme.icon_success);
        let _ = self.term.write_line(&format!("{} {}", icon, text));
    }

    pub fn warning(&self, text: &str) {
        self.capture(text);
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        let icon = self.theme.warning.apply_to(&self.theme.icon_warning);
        let styled_text = self.theme.warning.apply_to(text);
        let _ = self.term.write_line(&format!("{} {}", icon, styled_text));
    }

    pub fn error(&self, text: &str) {
        self.capture(text);
        let icon = self.theme.error.apply_to(&self.theme.icon_error);
        let styled_text = self.theme.error.apply_to(text);
        let _ = self.term.write_line(&format!("{} {}", icon, styled_text));
    }

    pub fn info(&self, text: &str) {
        self.capture(text);
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        let icon = self.theme.info.apply_to(&self.theme.icon_info);
        let styled_text = self.theme.info.apply_to(text);
        let _ = self.term.write_line(&format!("{} {}", icon, styled_text));
    }

    pub fn key_value(&self, key: &str, value: &str) {
        self.capture(&format!("{}: {}", key, value));
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        let k = self.theme.key.apply_to(format!("{:>16}", key));
        let arrow = self.theme.muted.apply_to(&self.theme.icon_arrow);
        let v = self.theme.value.apply_to(value);
        let _ = self.term.write_line(&format!("{} {} {}", k, arrow, v));
    }

    pub fn plan_phase(&self, name: &str, items: &[String]) {
        self.capture(&format!("Phase: {}", name));
        for item in items {
            self.capture(&format!("  {}", item));
        }
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
                let icon = self.theme.muted.apply_to(&self.theme.icon_pending);
                let muted_text = self.theme.muted.apply_to(item.as_str());
                let _ = self.term.write_line(&format!("  {} {}", icon, muted_text));
            }
        }
    }

    pub fn table(&self, headers: &[&str], rows: &[Vec<String>]) {
        if self.test_buf.is_some() {
            self.capture(&headers.join("\t"));
            for row in rows {
                self.capture(&row.join("\t"));
            }
        }
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
        if self.is_structured() {
            return Err(non_interactive_err(message));
        }
        inquire::Confirm::new(message).with_default(false).prompt()
    }

    pub fn prompt_select<'a>(
        &self,
        message: &str,
        options: &'a [String],
    ) -> std::result::Result<&'a String, inquire::InquireError> {
        if self.is_structured() {
            return Err(non_interactive_err(message));
        }
        if options.is_empty() {
            return Err(inquire::InquireError::Custom("no options available".into()));
        }
        let idx = inquire::Select::new(message, options.to_vec()).prompt()?;
        Ok(options.iter().find(|o| **o == idx).unwrap_or(&options[0]))
    }

    pub fn prompt_text(
        &self,
        message: &str,
        default: &str,
    ) -> std::result::Result<String, inquire::InquireError> {
        if self.is_structured() {
            return Err(non_interactive_err(message));
        }
        inquire::Text::new(message).with_default(default).prompt()
    }

    pub fn newline(&self) {
        let _ = self.term.write_line("");
    }

    /// Write a line to stdout (for machine-readable data output, not UI).
    /// Used by commands like `config get` whose output may be captured by scripts.
    pub fn stdout_line(&self, text: &str) {
        self.capture(text);
        let stdout = Term::stdout();
        let _ = stdout.write_line(text);
    }
}
