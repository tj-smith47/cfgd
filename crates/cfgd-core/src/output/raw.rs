//! Raw renderers — diff, syntax_highlight, data_line.
//!
//! Spec §5a: raw renderers are exempt from the indent invariant because
//! their content is multi-line and line-by-line indent would corrupt
//! syntax/diff output. All three render at depth 0.

use similar::{ChangeTag, TextDiff};
use syntect::easy::HighlightLines;
use syntect::highlighting::Style as SynStyle;
use syntect::parsing::SyntaxSet;
use syntect::util::as_24_bit_terminal_escaped;

use super::renderer::{Renderer, Writer};

impl Renderer {
    /// Render a unified diff using `theme.diff_*` styles. Lines starting with
    /// `+` are themed diff_add, `-` themed diff_remove, others diff_context.
    /// Always at depth 0 (raw renderer; see §5a).
    pub fn render_diff(&self, w: &dyn Writer, old: &str, new: &str) {
        let diff = TextDiff::from_lines(old, new);
        for change in diff.iter_all_changes() {
            let (sign, style) = match change.tag() {
                ChangeTag::Insert => ("+", &self.theme.diff_add),
                ChangeTag::Delete => ("-", &self.theme.diff_remove),
                ChangeTag::Equal => (" ", &self.theme.diff_context),
            };
            let body = format!("{sign}{change}");
            let body = body.trim_end_matches('\n');
            let styled = style.apply_to(body).to_string();
            w.write_line(&styled);
        }
    }

    /// Render syntax-highlighted code. Caller passes the `lang` hint (e.g.,
    /// "yaml", "rust", "json"); falls back to plain text on unknown.
    /// Always at depth 0 (raw renderer; see §5a).
    pub fn render_syntax_highlight(
        &self,
        w: &dyn Writer,
        code: &str,
        lang: &str,
        syntax_set: &SyntaxSet,
        theme_set: &syntect::highlighting::ThemeSet,
    ) {
        let syntax = syntax_set
            .find_syntax_by_token(lang)
            .or_else(|| syntax_set.find_syntax_by_extension(lang))
            .unwrap_or_else(|| syntax_set.find_syntax_plain_text());
        let Some(theme) = theme_set
            .themes
            .get("base16-ocean.dark")
            .or_else(|| theme_set.themes.values().next())
        else {
            // No syntect themes available; emit unstyled lines.
            for line in code.lines() {
                w.write_line(line);
            }
            return;
        };
        let mut h = HighlightLines::new(syntax, theme);
        for line in code.lines() {
            let ranges: Vec<(SynStyle, &str)> =
                h.highlight_line(line, syntax_set).unwrap_or_default();
            let escaped = as_24_bit_terminal_escaped(&ranges, false);
            w.write_line(&escaped);
        }
    }
}

impl super::Printer {
    /// Diff renderer (§5a raw). Goes to stderr.
    pub fn diff(&self, old: &str, new: &str) {
        self.renderer
            .render_diff(self.sink_stderr.as_ref(), old, new);
    }

    /// Syntax-highlighted code (§5a raw). Goes to stderr.
    pub fn syntax_highlight(&self, code: &str, lang: &str) {
        self.renderer.render_syntax_highlight(
            self.sink_stderr.as_ref(),
            code,
            lang,
            &self.syntax_set,
            &self.theme_set,
        );
    }

    /// Raw stdout line, no decoration, no indent (§5a). For `config get`-shaped
    /// callers whose output is consumed by other programs.
    pub fn data_line(&self, text: &str) {
        self.sink_stdout.write_line(text);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::super::renderer::StringSink;
    use super::super::{Theme, Verbosity};
    use super::*;
    use crate::output::tests::strip_ansi;

    #[test]
    fn diff_marks_changed_lines() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Normal);
        r.render_diff(&sink, "a\nb\nc\n", "a\nB\nc\n");
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(out.contains("-b"), "got: {out:?}");
        assert!(out.contains("+B"), "got: {out:?}");
    }

    #[test]
    fn syntax_highlight_renders_lines() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Normal);
        let ss = SyntaxSet::load_defaults_newlines();
        let ts = syntect::highlighting::ThemeSet::load_defaults();
        r.render_syntax_highlight(&sink, "let x = 1;\nlet y = 2;\n", "rs", &ss, &ts);
        let out = buf.lock().unwrap();
        let stripped = strip_ansi(&out);
        assert!(
            stripped.contains("let x"),
            "stripped output missing 'let x': {stripped:?}"
        );
        assert!(
            stripped.contains("let y"),
            "stripped output missing 'let y': {stripped:?}"
        );
    }

    #[test]
    fn data_line_writes_to_stdout_raw() {
        use super::super::Verbosity;
        use super::super::printer::Printer;

        let stdout_buf = Arc::new(Mutex::new(String::new()));
        let stderr_buf = Arc::new(Mutex::new(String::new()));
        let mut p = Printer::new(Verbosity::Normal);
        // Swap in capture sinks.
        p.sink_stdout = Arc::new(StringSink(stdout_buf.clone()));
        p.sink_stderr = Arc::new(StringSink(stderr_buf.clone()));

        p.data_line("raw payload");
        p.flush();

        let stdout = stdout_buf.lock().unwrap();
        let stderr = stderr_buf.lock().unwrap();
        // data_line is RAW: exact text on stdout, no decoration, no indent.
        assert!(stdout.contains("raw payload"), "stdout got: {stdout:?}");
        // And NOT routed through the section/indent system to stderr.
        assert!(
            !stderr.contains("raw payload"),
            "leaked to stderr: {stderr:?}"
        );
    }
}
