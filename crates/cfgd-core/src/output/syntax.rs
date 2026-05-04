use syntect::easy::HighlightLines;
use syntect::util::as_24_bit_terminal_escaped;

use super::{Printer, Verbosity};

impl Printer {
    pub fn syntax_highlight(&self, code: &str, language: &str) {
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        let syntax = self
            .syntax_set
            .find_syntax_by_token(language)
            .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text());
        let theme = match self.theme_set.themes.get("base16-ocean.dark") {
            Some(t) => t,
            None => return, // no theme available — skip highlighting
        };
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
}
