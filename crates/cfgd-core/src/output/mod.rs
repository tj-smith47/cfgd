// Centralized output system — all terminal interaction goes through Printer

use console::{Color, Style, Term};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use serde::Serialize;
use similar::{ChangeTag, TextDiff};
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use syntect::util::as_24_bit_terminal_escaped;

use std::collections::VecDeque;
use std::io::BufRead;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use crate::config::ThemeConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputFormat {
    Table,
    Wide,
    Json,
    Yaml,
    Name,
    Jsonpath(String),
    Template(String),
    TemplateFile(std::path::PathBuf),
}

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

/// Apply a kubectl-compatible jsonpath expression to a JSON value.
///
/// Supports:
/// - `{.field.nested}` — dot-path into objects
/// - `{.items[0]}` — array index
/// - `{.items[*].name}` — wildcard collect
/// - `{.items[0:3]}` — array slice
///
/// Scalars return as raw text, objects/arrays as JSON.
fn apply_jsonpath(value: &serde_json::Value, expr: &str) -> String {
    // Strip optional { } wrapper
    let expr = expr.trim();
    let expr = expr.strip_prefix('{').unwrap_or(expr);
    let expr = expr.strip_suffix('}').unwrap_or(expr);
    let expr = expr.strip_prefix('.').unwrap_or(expr);

    if expr.is_empty() {
        return format_jsonpath_result(value);
    }

    let results = walk_jsonpath(value, expr);
    match results.len() {
        0 => String::new(),
        1 => format_jsonpath_result(results[0]),
        _ => {
            // Multiple results: output each on its own line (kubectl behavior)
            results
                .iter()
                .map(|v| format_jsonpath_result(v))
                .collect::<Vec<_>>()
                .join("\n")
        }
    }
}

/// Extract a name-like identity field from a JSON value.
/// Tries "name" first, then common identity fields as fallbacks.
fn name_from_value(value: &serde_json::Value) -> Option<String> {
    for key in &["name", "context", "phase", "resourceType", "url"] {
        if let Some(s) = value.get(key).and_then(|v| v.as_str()) {
            return Some(s.to_string());
        }
    }
    // Try numeric identity fields
    for key in &["applyId"] {
        if let Some(n) = value.get(key).and_then(|v| v.as_i64()) {
            return Some(n.to_string());
        }
    }
    None
}

fn walk_jsonpath<'a>(value: &'a serde_json::Value, path: &str) -> Vec<&'a serde_json::Value> {
    if path.is_empty() {
        return vec![value];
    }

    // Parse the next segment: either `key`, `key[index]`, `key[*]`, or `key[start:end]`
    let (segment, rest) = split_jsonpath_segment(path);

    // Check for array access on the segment
    if let Some(bracket_pos) = segment.find('[') {
        let key = &segment[..bracket_pos];
        let bracket_expr = &segment[bracket_pos + 1..segment.len() - 1]; // strip [ ]

        // Navigate to the key first (if non-empty)
        let target = if key.is_empty() {
            value
        } else {
            match value.get(key) {
                Some(v) => v,
                None => return vec![],
            }
        };

        let arr = match target.as_array() {
            Some(a) => a,
            None => return vec![],
        };

        if bracket_expr == "*" {
            // Wildcard: collect from all elements
            arr.iter()
                .flat_map(|elem| walk_jsonpath(elem, rest))
                .collect()
        } else if let Some(colon_pos) = bracket_expr.find(':') {
            // Slice: [start:end]
            let start = bracket_expr[..colon_pos]
                .parse::<usize>()
                .unwrap_or(0)
                .min(arr.len());
            let end = bracket_expr[colon_pos + 1..]
                .parse::<usize>()
                .unwrap_or(arr.len())
                .min(arr.len());
            if start >= end {
                vec![]
            } else if rest.is_empty() {
                arr[start..end].iter().collect()
            } else {
                arr[start..end]
                    .iter()
                    .flat_map(|elem| walk_jsonpath(elem, rest))
                    .collect()
            }
        } else {
            // Numeric index
            let idx: usize = match bracket_expr.parse() {
                Ok(i) => i,
                Err(_) => return vec![],
            };
            match arr.get(idx) {
                Some(elem) => walk_jsonpath(elem, rest),
                None => vec![],
            }
        }
    } else {
        // Plain key access
        match value.get(segment) {
            Some(v) => walk_jsonpath(v, rest),
            None => vec![],
        }
    }
}

/// Split a jsonpath into the first segment and the rest.
/// Handles bracket notation: `items[0].name` → (`items[0]`, `name`)
fn split_jsonpath_segment(path: &str) -> (&str, &str) {
    let mut in_bracket = false;
    for (i, c) in path.char_indices() {
        match c {
            '[' => in_bracket = true,
            ']' => in_bracket = false,
            '.' if !in_bracket => {
                return (&path[..i], &path[i + 1..]);
            }
            _ => {}
        }
    }
    (path, "")
}

/// Format a jsonpath result value: scalars as raw text, objects/arrays as JSON.
fn format_jsonpath_result(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => serde_json::to_string_pretty(value).unwrap_or_default(),
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
    output_format: OutputFormat,
    /// Optional buffer for capturing output in tests. When set, all output
    /// methods append plain text here regardless of verbosity level.
    test_buf: Option<Arc<Mutex<String>>>,
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

    pub fn with_format(
        verbosity: Verbosity,
        theme_config: Option<&ThemeConfig>,
        output_format: OutputFormat,
    ) -> Self {
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
    fn capture(&self, text: &str) {
        if let Some(ref buf) = self.test_buf {
            let mut b = buf.lock().unwrap();
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

    pub fn diff(&self, old: &str, new: &str) {
        if self.test_buf.is_some() {
            let diff = TextDiff::from_lines(old, new);
            for change in diff.iter_all_changes() {
                let sign = match change.tag() {
                    ChangeTag::Delete => "-",
                    ChangeTag::Insert => "+",
                    ChangeTag::Equal => " ",
                };
                self.capture(&format!("{}{}", sign, change.to_string().trim_end()));
            }
        }
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

    pub fn spinner(&self, message: &str) -> ProgressBar {
        if self.verbosity == Verbosity::Quiet {
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
        inquire::Confirm::new(message).with_default(false).prompt()
    }

    pub fn prompt_select<'a>(
        &self,
        message: &str,
        options: &'a [String],
    ) -> std::result::Result<&'a String, inquire::InquireError> {
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

    /// Whether structured output mode is active (not table or wide).
    pub fn is_structured(&self) -> bool {
        !matches!(self.output_format, OutputFormat::Table | OutputFormat::Wide)
    }

    /// Returns `true` when `-o wide` was specified, enabling extra columns.
    pub fn is_wide(&self) -> bool {
        matches!(self.output_format, OutputFormat::Wide)
    }

    /// Write a serializable value as structured output to stdout.
    /// Returns `true` if output was emitted (caller should skip human formatting).
    /// Returns `false` if output format is Table/Wide (caller should do human formatting).
    pub fn write_structured<T: Serialize>(&self, value: &T) -> bool {
        match &self.output_format {
            OutputFormat::Table | OutputFormat::Wide => false,
            OutputFormat::Json => {
                let json_value = serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
                let text = serde_json::to_string_pretty(&json_value).unwrap_or_default();
                self.stdout_line(&text);
                true
            }
            OutputFormat::Yaml => {
                let yaml = serde_yaml::to_string(value).unwrap_or_default();
                let trimmed = yaml.strip_prefix("---\n").unwrap_or(&yaml);
                self.stdout_line(trimmed.trim_end());
                true
            }
            OutputFormat::Name => {
                let json_value = serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
                match &json_value {
                    serde_json::Value::Array(arr) => {
                        for item in arr {
                            if let Some(name) = name_from_value(item) {
                                self.stdout_line(&name);
                            }
                        }
                    }
                    obj => {
                        if let Some(name) = name_from_value(obj) {
                            self.stdout_line(&name);
                        }
                    }
                }
                true
            }
            OutputFormat::Jsonpath(expr) => {
                let json_value = serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
                let text = apply_jsonpath(&json_value, expr);
                self.stdout_line(&text);
                true
            }
            OutputFormat::Template(tmpl) => {
                self.render_template(value, tmpl);
                true
            }
            OutputFormat::TemplateFile(path) => {
                let path = crate::expand_tilde(path);
                match std::fs::read_to_string(&path) {
                    Ok(tmpl) => {
                        self.render_template(value, &tmpl);
                    }
                    Err(e) => {
                        self.error(&format!(
                            "failed to read template file '{}': {}",
                            path.display(),
                            e
                        ));
                    }
                }
                true
            }
        }
    }

    fn render_template<T: Serialize>(&self, value: &T, template: &str) {
        let json_value = serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
        let mut tera = tera::Tera::default();
        let tmpl_name = "__inline__";
        if let Err(e) = tera.add_raw_template(tmpl_name, template) {
            self.error(&format!("invalid template: {}", e));
            return;
        }
        match &json_value {
            serde_json::Value::Array(arr) => {
                for item in arr {
                    match tera::Context::from_value(item.clone()) {
                        Ok(ctx) => match tera.render(tmpl_name, &ctx) {
                            Ok(rendered) => self.stdout_line(&rendered),
                            Err(e) => self.error(&format!("template render error: {}", e)),
                        },
                        Err(e) => self.error(&format!("template context error: {}", e)),
                    }
                }
            }
            other => match tera::Context::from_value(other.clone()) {
                Ok(ctx) => match tera.render(tmpl_name, &ctx) {
                    Ok(rendered) => self.stdout_line(&rendered),
                    Err(e) => self.error(&format!("template render error: {}", e)),
                },
                Err(e) => self.error(&format!("template context error: {}", e)),
            },
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printer_respects_quiet_verbosity() {
        let printer = Printer::new(Verbosity::Quiet);
        assert_eq!(printer.verbosity(), Verbosity::Quiet);
        printer.header("test");
        printer.success("test");
        printer.warning("test");
        printer.info("test");
        printer.key_value("key", "value");
    }

    #[test]
    fn printer_error_always_prints() {
        // Errors go to stderr which can't easily be captured in unit tests,
        // so we verify it doesn't panic when called at Quiet verbosity.
        let printer = Printer::new(Verbosity::Quiet);
        printer.error("this is an error");
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
        let default = Theme::from_preset("default");
        let dracula = Theme::from_preset("dracula");
        let solarized_dark = Theme::from_preset("solarized-dark");
        let solarized_light = Theme::from_preset("solarized-light");
        let minimal = Theme::from_preset("minimal");
        // Presets should produce distinct themes
        assert_ne!(default.icon_success, minimal.icon_success);
        assert_ne!(dracula.icon_success, minimal.icon_success);
        assert_ne!(solarized_dark.icon_success, minimal.icon_success);
        assert_ne!(solarized_light.icon_success, minimal.icon_success);
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
    fn run_with_output_captures_stdout() {
        let printer = Printer::new(Verbosity::Quiet);
        let output = printer
            .run_with_output(std::process::Command::new("echo").arg("hello"), "test echo")
            .unwrap();
        assert!(output.status.success());
        assert!(output.stdout.contains("hello"));
    }

    #[test]
    #[cfg(unix)]
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
    #[cfg(unix)]
    fn run_with_output_reports_failure() {
        let printer = Printer::new(Verbosity::Quiet);
        let output = printer
            .run_with_output(&mut std::process::Command::new("false"), "test failure")
            .unwrap();
        assert!(!output.status.success());
    }

    #[test]
    #[cfg(unix)]
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
        match result {
            Err(err) => assert_eq!(err.kind(), std::io::ErrorKind::NotFound),
            Ok(_) => panic!("expected spawn error for nonexistent binary"),
        }
    }

    // --- OutputFormat and write_structured tests ---

    #[test]
    fn output_format_table_returns_false() {
        let printer = Printer::new(Verbosity::Normal);
        assert!(!printer.is_structured());
        let val = serde_json::json!({"key": "value"});
        assert!(!printer.write_structured(&val));
    }

    #[test]
    fn output_format_json_returns_true() {
        let printer = Printer::with_format(Verbosity::Normal, None, OutputFormat::Json);
        assert!(printer.is_structured());
        let val = serde_json::json!({"key": "value"});
        assert!(printer.write_structured(&val));
    }

    #[test]
    fn output_format_yaml_returns_true() {
        let printer = Printer::with_format(Verbosity::Normal, None, OutputFormat::Yaml);
        assert!(printer.is_structured());
        assert!(printer.write_structured(&"hello"));
    }

    // --- jsonpath tests ---

    #[test]
    fn apply_jsonpath_cases() {
        let obj = serde_json::json!({"name": "cfgd", "version": "1.0"});
        let nested = serde_json::json!({"status": {"phase": "running", "ready": true}});
        let arr = serde_json::json!({"items": ["a", "b", "c"]});
        let arr_obj = serde_json::json!({"items": [{"name": "a"}, {"name": "b"}]});
        let arr_num = serde_json::json!({"items": [1, 2, 3, 4, 5]});
        let arr_short = serde_json::json!({"items": [1, 2]});
        let arr3 = serde_json::json!({"items": [1, 2, 3]});
        let null_val = serde_json::json!({"key": null});
        let small = serde_json::json!({"a": 1});

        // Simple scalar lookups
        let cases: &[(&serde_json::Value, &str, &str)] = &[
            (&obj, "{.name}", "cfgd"),
            (&obj, "{.version}", "1.0"),
            (&nested, "{.status.phase}", "running"),
            (&nested, "{.status.ready}", "true"),
            (&arr, "{.items[0]}", "a"),
            (&arr, "{.items[2]}", "c"),
            (&arr_obj, "{.items[*].name}", "a\nb"),
            (&arr_num, "{.items[1:3]}", "2\n3"),
            (&obj, "{.missing}", ""),
            (&obj, ".name", "cfgd"),
            (&arr_short, "{.items[5]}", ""),
            (&arr3, "{.items[10:20]}", ""),
            (&arr3, "{.items[5:2]}", ""),
            (&null_val, "{.key}", ""),
        ];
        for (val, expr, expected) in cases {
            assert_eq!(
                apply_jsonpath(val, expr),
                *expected,
                "failed for expr {expr:?}"
            );
        }

        // Object/full-value results (need JSON parsing)
        let status_json = apply_jsonpath(&nested, "{.status}");
        let parsed: serde_json::Value = serde_json::from_str(&status_json).unwrap();
        assert_eq!(parsed["phase"], "running");

        let full = apply_jsonpath(&small, "{}");
        let parsed: serde_json::Value = serde_json::from_str(&full).unwrap();
        assert_eq!(parsed["a"], 1);
    }

    #[test]
    fn split_jsonpath_segment_simple() {
        assert_eq!(split_jsonpath_segment("foo.bar"), ("foo", "bar"));
        assert_eq!(split_jsonpath_segment("foo"), ("foo", ""));
    }

    #[test]
    fn split_jsonpath_segment_with_bracket() {
        assert_eq!(
            split_jsonpath_segment("items[0].name"),
            ("items[0]", "name")
        );
        assert_eq!(
            split_jsonpath_segment("items[*].name"),
            ("items[*]", "name")
        );
    }

    // Smoke test: verify all printer methods execute without panic
    #[test]
    fn printer_methods_smoke_test() {
        let printer = Printer::new(Verbosity::Quiet);
        printer.subheader("Test Section");
        printer.newline();
        printer.stdout_line("output line");

        // Table with data and empty
        let rows = vec![
            vec!["a".to_string(), "b".to_string()],
            vec!["c".to_string(), "d".to_string()],
        ];
        printer.table(&["Col1", "Col2"], &rows);
        let empty_rows: Vec<Vec<String>> = vec![];
        printer.table(&["Col1"], &empty_rows);

        // Plan phase with items and empty
        printer.plan_phase("Packages", &["install brew: curl".to_string()]);
        printer.plan_phase("Files", &[]);

        // Diff with changes and identical
        printer.diff("old content\nline2", "new content\nline2");
        printer.diff("same", "same");

        // Syntax highlighting with known and unknown language
        printer.syntax_highlight("fn main() {}", "rs");
        printer.syntax_highlight("some text", "unknown_lang_xyz");

        // write_structured in non-structured mode
        let data = serde_json::json!({"key": "value"});
        printer.write_structured(&data);
    }

    // --- write_structured format variants ---

    #[test]
    fn write_structured_name_single_object() {
        let printer = Printer::with_format(Verbosity::Normal, None, OutputFormat::Name);
        assert!(printer.is_structured());
        let val = serde_json::json!({"name": "my-profile"});
        assert!(printer.write_structured(&val));
    }

    #[test]
    fn write_structured_name_array() {
        let printer = Printer::with_format(Verbosity::Normal, None, OutputFormat::Name);
        let val = serde_json::json!([
            {"name": "profile-a"},
            {"name": "profile-b"}
        ]);
        assert!(printer.write_structured(&val));
    }

    #[test]
    fn write_structured_name_fallback_fields() {
        // name_from_value tries "name", then "context", "phase", "resourceType", "url", "applyId"
        let printer = Printer::with_format(Verbosity::Normal, None, OutputFormat::Name);

        let context_val = serde_json::json!({"context": "production"});
        assert!(printer.write_structured(&context_val));

        let phase_val = serde_json::json!({"phase": "Packages"});
        assert!(printer.write_structured(&phase_val));

        let apply_id_val = serde_json::json!({"applyId": 42});
        assert!(printer.write_structured(&apply_id_val));
    }

    #[test]
    fn write_structured_jsonpath() {
        let printer = Printer::with_format(
            Verbosity::Normal,
            None,
            OutputFormat::Jsonpath("{.status.phase}".to_string()),
        );
        assert!(printer.is_structured());
        let val = serde_json::json!({"status": {"phase": "ready"}});
        assert!(printer.write_structured(&val));
    }

    #[test]
    fn write_structured_template() {
        let printer = Printer::with_format(
            Verbosity::Normal,
            None,
            OutputFormat::Template("Name: {{ name }}".to_string()),
        );
        assert!(printer.is_structured());
        let val = serde_json::json!({"name": "my-config"});
        assert!(printer.write_structured(&val));
    }

    #[test]
    fn write_structured_template_array() {
        let printer = Printer::with_format(
            Verbosity::Normal,
            None,
            OutputFormat::Template("- {{ name }}".to_string()),
        );
        let val = serde_json::json!([
            {"name": "a"},
            {"name": "b"}
        ]);
        assert!(printer.write_structured(&val));
    }

    #[test]
    fn write_structured_template_file() {
        let dir = tempfile::tempdir().unwrap();
        let tmpl_path = dir.path().join("output.tmpl");
        std::fs::write(&tmpl_path, "Name={{ name }} Version={{ version }}").unwrap();

        let printer = Printer::with_format(
            Verbosity::Normal,
            None,
            OutputFormat::TemplateFile(tmpl_path),
        );
        let val = serde_json::json!({"name": "cfgd", "version": "1.0"});
        assert!(printer.write_structured(&val));
    }

    #[test]
    fn write_structured_template_file_missing() {
        let printer = Printer::with_format(
            Verbosity::Normal,
            None,
            OutputFormat::TemplateFile("/nonexistent/template.tmpl".into()),
        );
        let val = serde_json::json!({"key": "value"});
        // Should return true (structured output mode) but print error
        assert!(printer.write_structured(&val));
    }

    #[test]
    fn write_structured_wide_returns_false() {
        let printer = Printer::with_format(Verbosity::Normal, None, OutputFormat::Wide);
        assert!(!printer.is_structured());
        assert!(printer.is_wide());
        let val = serde_json::json!({"key": "value"});
        assert!(!printer.write_structured(&val));
    }

    // --- name_from_value edge cases ---

    #[test]
    fn name_from_value_no_match_returns_none() {
        let val = serde_json::json!({"unknown_field": "value"});
        assert!(name_from_value(&val).is_none());
    }

    #[test]
    fn name_from_value_prefers_name_over_others() {
        let val = serde_json::json!({"name": "primary", "context": "secondary"});
        assert_eq!(name_from_value(&val), Some("primary".to_string()));
    }

    #[test]
    fn name_from_value_numeric_apply_id() {
        let val = serde_json::json!({"applyId": 123});
        assert_eq!(name_from_value(&val), Some("123".to_string()));
    }

    #[test]
    fn name_from_value_null_returns_none() {
        assert!(name_from_value(&serde_json::Value::Null).is_none());
    }

    // --- Theme construction edge cases ---

    #[test]
    fn theme_from_config_all_icon_overrides() {
        let config = ThemeConfig {
            name: "default".into(),
            overrides: crate::config::ThemeOverrides {
                icon_success: Some("OK".into()),
                icon_warning: Some("!!".into()),
                icon_error: Some("ERR".into()),
                icon_info: Some("ii".into()),
                icon_pending: Some("..".into()),
                icon_arrow: Some(">>".into()),
                ..Default::default()
            },
        };
        let theme = Theme::from_config(Some(&config));
        assert_eq!(theme.icon_success, "OK");
        assert_eq!(theme.icon_warning, "!!");
        assert_eq!(theme.icon_error, "ERR");
        assert_eq!(theme.icon_info, "ii");
        assert_eq!(theme.icon_pending, "..");
        assert_eq!(theme.icon_arrow, ">>");
    }

    #[test]
    fn theme_from_config_all_color_overrides() {
        let config = ThemeConfig {
            name: "default".into(),
            overrides: crate::config::ThemeOverrides {
                success: Some("#00ff00".into()),
                warning: Some("#ffff00".into()),
                error: Some("#ff0000".into()),
                info: Some("#00ffff".into()),
                muted: Some("#888888".into()),
                header: Some("#0000ff".into()),
                subheader: Some("#ff00ff".into()),
                key: Some("#ff79c6".into()),
                value: Some("#f8f8f2".into()),
                diff_add: Some("#50fa7b".into()),
                diff_remove: Some("#ff5555".into()),
                diff_context: Some("#6272a4".into()),
                ..Default::default()
            },
        };
        // Should not panic — all colors are applied
        let _theme = Theme::from_config(Some(&config));
    }

    #[test]
    fn theme_from_config_invalid_color_ignored() {
        let config = ThemeConfig {
            name: "default".into(),
            overrides: crate::config::ThemeOverrides {
                success: Some("not-a-color".into()),
                ..Default::default()
            },
        };
        // Should not panic — invalid color is silently ignored
        let _theme = Theme::from_config(Some(&config));
    }

    // --- format_jsonpath_result ---

    #[test]
    fn format_jsonpath_result_bool() {
        assert_eq!(
            format_jsonpath_result(&serde_json::Value::Bool(true)),
            "true"
        );
        assert_eq!(
            format_jsonpath_result(&serde_json::Value::Bool(false)),
            "false"
        );
    }

    #[test]
    fn format_jsonpath_result_number() {
        let num = serde_json::json!(42);
        assert_eq!(format_jsonpath_result(&num), "42");
        let float = serde_json::json!(1.234);
        assert_eq!(format_jsonpath_result(&float), "1.234");
    }

    #[test]
    fn format_jsonpath_result_null_empty() {
        assert_eq!(format_jsonpath_result(&serde_json::Value::Null), "");
    }

    // --- Printer method behavior ---

    #[test]
    fn printer_normal_verbosity_methods_do_not_panic() {
        // Unlike Quiet mode (which skips), Normal mode exercises the full rendering path
        let printer = Printer::new(Verbosity::Normal);

        printer.header("Test Header");
        printer.subheader("Test Subheader");
        printer.success("Operation succeeded");
        printer.warning("Something might be wrong");
        printer.error("Something failed");
        printer.info("Some information");
        printer.key_value("Profile", "developer");
        printer.key_value("Config Dir", "~/.config/cfgd");
        printer.newline();

        // Table with real data
        printer.table(
            &["NAME", "STATUS", "AGE"],
            &[
                vec!["nvim".into(), "installed".into(), "2d".into()],
                vec!["tmux".into(), "missing".into(), "-".into()],
            ],
        );

        // Plan phase
        printer.plan_phase(
            "Packages",
            &[
                "install brew: neovim".to_string(),
                "install apt: ripgrep".to_string(),
            ],
        );
        printer.plan_phase("Files", &[]);

        // Diff
        printer.diff("line1\nline2\nline3", "line1\nmodified\nline3");
        printer.diff("identical", "identical");

        // Syntax highlight
        printer.syntax_highlight("fn main() { println!(\"hello\"); }", "rs");
    }

    #[test]
    fn spinner_hidden_in_quiet_mode() {
        let printer = Printer::new(Verbosity::Quiet);
        let spinner = printer.spinner("loading...");
        // ProgressBar::hidden() is returned in quiet mode — verify it doesn't panic
        spinner.finish_and_clear();
    }

    #[test]
    fn progress_bar_creates_valid_bar() {
        let printer = Printer::new(Verbosity::Normal);
        let pb = printer.progress_bar(100, "processing");
        pb.inc(50);
        assert_eq!(pb.position(), 50);
        pb.finish();
    }

    #[test]
    fn disable_colors_does_not_panic() {
        Printer::disable_colors();
    }

    // --- jsonpath walk edge cases ---

    #[test]
    fn jsonpath_non_array_bracket_access_returns_empty() {
        let val = serde_json::json!({"items": "not-an-array"});
        assert_eq!(apply_jsonpath(&val, "{.items[0]}"), "");
    }

    #[test]
    fn jsonpath_non_numeric_bracket_returns_empty() {
        let val = serde_json::json!({"items": [1, 2, 3]});
        assert_eq!(apply_jsonpath(&val, "{.items[abc]}"), "");
    }

    #[test]
    fn jsonpath_nested_array_wildcard() {
        let val = serde_json::json!({
            "groups": [
                {"members": [{"name": "alice"}, {"name": "bob"}]},
                {"members": [{"name": "charlie"}]}
            ]
        });
        let result = apply_jsonpath(&val, "{.groups[*].members[*].name}");
        assert!(result.contains("alice"), "should contain alice: {result}");
        assert!(result.contains("bob"), "should contain bob: {result}");
        assert!(
            result.contains("charlie"),
            "should contain charlie: {result}"
        );
    }

    #[test]
    fn jsonpath_slice_from_start() {
        let val = serde_json::json!({"items": ["a", "b", "c", "d"]});
        // [0:2] should return first two
        assert_eq!(apply_jsonpath(&val, "{.items[0:2]}"), "a\nb");
    }

    #[test]
    fn jsonpath_empty_array() {
        let val = serde_json::json!({"items": []});
        assert_eq!(apply_jsonpath(&val, "{.items[0]}"), "");
        assert_eq!(apply_jsonpath(&val, "{.items[*]}"), "");
        assert_eq!(apply_jsonpath(&val, "{.items[0:5]}"), "");
    }

    // --- Printer::for_test capture behavior ---

    #[test]
    fn for_test_captures_header_text() {
        let (printer, buf) = Printer::for_test();
        printer.header("Test Header");
        let output = buf.lock().unwrap();
        assert!(output.contains("Test Header"), "should capture header text");
    }

    #[test]
    fn for_test_captures_success_text() {
        let (printer, buf) = Printer::for_test();
        printer.success("Operation passed");
        let output = buf.lock().unwrap();
        assert!(
            output.contains("Operation passed"),
            "should capture success text"
        );
    }

    #[test]
    fn for_test_captures_warning_text() {
        let (printer, buf) = Printer::for_test();
        printer.warning("Careful now");
        let output = buf.lock().unwrap();
        assert!(
            output.contains("Careful now"),
            "should capture warning text"
        );
    }

    #[test]
    fn for_test_captures_error_text() {
        let (printer, buf) = Printer::for_test();
        printer.error("Something broke");
        let output = buf.lock().unwrap();
        assert!(
            output.contains("Something broke"),
            "should capture error text"
        );
    }

    #[test]
    fn for_test_captures_info_text() {
        let (printer, buf) = Printer::for_test();
        printer.info("FYI message");
        let output = buf.lock().unwrap();
        assert!(output.contains("FYI message"), "should capture info text");
    }

    #[test]
    fn for_test_captures_key_value() {
        let (printer, buf) = Printer::for_test();
        printer.key_value("Profile", "developer");
        let output = buf.lock().unwrap();
        assert!(
            output.contains("Profile") && output.contains("developer"),
            "should capture key and value"
        );
    }

    #[test]
    fn for_test_captures_subheader() {
        let (printer, buf) = Printer::for_test();
        printer.subheader("Subsection");
        let output = buf.lock().unwrap();
        assert!(
            output.contains("Subsection"),
            "should capture subheader text"
        );
    }

    #[test]
    fn for_test_captures_plan_phase() {
        let (printer, buf) = Printer::for_test();
        printer.plan_phase("Install", &["neovim".to_string(), "tmux".to_string()]);
        let output = buf.lock().unwrap();
        assert!(
            output.contains("Phase: Install"),
            "should capture phase name"
        );
        assert!(
            output.contains("neovim") && output.contains("tmux"),
            "should capture phase items"
        );
    }

    #[test]
    fn for_test_captures_plan_phase_empty() {
        let (printer, buf) = Printer::for_test();
        printer.plan_phase("Cleanup", &[]);
        let output = buf.lock().unwrap();
        assert!(
            output.contains("Phase: Cleanup"),
            "should capture empty phase name"
        );
    }

    #[test]
    fn for_test_captures_table() {
        let (printer, buf) = Printer::for_test();
        let rows = vec![
            vec!["nvim".to_string(), "installed".to_string()],
            vec!["tmux".to_string(), "missing".to_string()],
        ];
        printer.table(&["NAME", "STATUS"], &rows);
        let output = buf.lock().unwrap();
        assert!(
            output.contains("NAME") && output.contains("STATUS"),
            "should capture table headers"
        );
        assert!(
            output.contains("nvim") && output.contains("installed"),
            "should capture table rows"
        );
        assert!(
            output.contains("tmux") && output.contains("missing"),
            "should capture second row"
        );
    }

    #[test]
    fn for_test_captures_diff() {
        let (printer, buf) = Printer::for_test();
        printer.diff("line1\nold\nline3\n", "line1\nnew\nline3\n");
        let output = buf.lock().unwrap();
        assert!(output.contains("-old"), "should capture removed line");
        assert!(output.contains("+new"), "should capture added line");
        assert!(
            output.contains(" line1"),
            "should capture unchanged context"
        );
    }

    #[test]
    fn for_test_captures_diff_identical() {
        let (printer, buf) = Printer::for_test();
        printer.diff("same\n", "same\n");
        let output = buf.lock().unwrap();
        assert!(
            output.contains(" same"),
            "identical diff should show context line"
        );
        assert!(
            !output.contains("-same") && !output.contains("+same"),
            "identical diff should have no add/remove markers"
        );
    }

    #[test]
    fn for_test_captures_stdout_line() {
        let (printer, buf) = Printer::for_test();
        printer.stdout_line("data output");
        let output = buf.lock().unwrap();
        assert!(
            output.contains("data output"),
            "should capture stdout_line text"
        );
    }

    // --- write_structured with for_test ---

    #[test]
    fn for_test_write_structured_json() {
        let (printer, buf) = Printer::for_test_with_format(OutputFormat::Json);
        let val = serde_json::json!({"key": "value"});
        let wrote = printer.write_structured(&val);
        assert!(wrote, "should return true for JSON format");
        let output = buf.lock().unwrap();
        assert!(
            output.contains("key") && output.contains("value"),
            "should capture JSON output"
        );
    }

    #[test]
    fn for_test_write_structured_yaml() {
        let (printer, buf) = Printer::for_test_with_format(OutputFormat::Yaml);
        let val = serde_json::json!({"name": "test"});
        let wrote = printer.write_structured(&val);
        assert!(wrote, "should return true for YAML format");
        let output = buf.lock().unwrap();
        assert!(
            output.contains("name") && output.contains("test"),
            "should capture YAML output"
        );
    }

    #[test]
    fn for_test_write_structured_name() {
        let (printer, buf) = Printer::for_test_with_format(OutputFormat::Name);
        let val = serde_json::json!({"name": "my-profile"});
        let wrote = printer.write_structured(&val);
        assert!(wrote, "should return true for Name format");
        let output = buf.lock().unwrap();
        assert!(output.contains("my-profile"), "should capture name output");
    }

    #[test]
    fn for_test_write_structured_name_array_items() {
        let (printer, buf) = Printer::for_test_with_format(OutputFormat::Name);
        let val = serde_json::json!([
            {"name": "alpha"},
            {"name": "beta"}
        ]);
        let wrote = printer.write_structured(&val);
        assert!(wrote);
        let output = buf.lock().unwrap();
        assert!(output.contains("alpha"), "should capture first name");
        assert!(output.contains("beta"), "should capture second name");
    }

    #[test]
    fn for_test_write_structured_jsonpath_captures() {
        let (printer, buf) =
            Printer::for_test_with_format(OutputFormat::Jsonpath("{.name}".to_string()));
        let val = serde_json::json!({"name": "cfgd", "version": "2.0"});
        let wrote = printer.write_structured(&val);
        assert!(wrote);
        let output = buf.lock().unwrap();
        assert!(output.contains("cfgd"), "should capture jsonpath result");
    }

    #[test]
    fn for_test_write_structured_template_captures() {
        let (printer, buf) =
            Printer::for_test_with_format(OutputFormat::Template("Hello {{ name }}!".to_string()));
        let val = serde_json::json!({"name": "world"});
        let wrote = printer.write_structured(&val);
        assert!(wrote);
        let output = buf.lock().unwrap();
        assert!(
            output.contains("Hello world!"),
            "should capture rendered template"
        );
    }

    // --- ansi256_from_rgb edge cases ---

    #[test]
    fn ansi256_from_rgb_pure_black() {
        // r==g==b==0, r < 8 -> should return 16
        assert_eq!(ansi256_from_rgb(0, 0, 0), 16);
    }

    #[test]
    fn ansi256_from_rgb_near_white() {
        // r==g==b==255, r > 248 -> should return 231
        assert_eq!(ansi256_from_rgb(255, 255, 255), 231);
    }

    #[test]
    fn ansi256_from_rgb_grayscale_midrange() {
        // r==g==b, 8 <= r <= 248 -> grayscale ramp 232-255
        let idx = ansi256_from_rgb(128, 128, 128);
        assert!(
            idx >= 232 && idx <= 255,
            "midrange gray should be in grayscale ramp: {idx}"
        );
    }

    #[test]
    fn ansi256_from_rgb_color_cube() {
        // r!=g or g!=b -> 6x6x6 color cube (16-231)
        let idx = ansi256_from_rgb(255, 0, 0); // pure red
        assert!(
            idx >= 16 && idx <= 231,
            "pure red should map to color cube: {idx}"
        );
    }

    #[test]
    fn ansi256_from_rgb_various_colors() {
        // Verify no panics and range validity for several colors
        let colors: &[(u8, u8, u8)] = &[
            (0, 255, 0),    // green
            (0, 0, 255),    // blue
            (128, 64, 0),   // brown
            (200, 200, 50), // yellow-ish (not all equal -> cube)
        ];
        for (r, g, b) in colors {
            let idx = ansi256_from_rgb(*r, *g, *b);
            assert!(
                idx >= 16 && idx <= 231,
                "color ({r},{g},{b}) should map to cube or grayscale: {idx}"
            );
        }
    }

    // --- Template rendering error paths ---

    #[test]
    fn write_structured_invalid_template_syntax() {
        let (printer, buf) = Printer::for_test_with_format(OutputFormat::Template(
            "{{ invalid {% endfor }".to_string(),
        ));
        let val = serde_json::json!({"name": "test"});
        let wrote = printer.write_structured(&val);
        assert!(wrote, "should still return true");
        let output = buf.lock().unwrap();
        assert!(
            output.contains("invalid template"),
            "should capture template error message, got: {output}"
        );
    }

    // --- OutputFormat variant coverage ---

    #[test]
    fn output_format_auto_quiets_structured() {
        // When structured output is active, verbosity should be set to Quiet
        let printer = Printer::with_format(Verbosity::Normal, None, OutputFormat::Json);
        assert_eq!(printer.verbosity(), Verbosity::Quiet);
    }

    #[test]
    fn output_format_table_preserves_verbosity() {
        let printer = Printer::with_format(Verbosity::Normal, None, OutputFormat::Table);
        assert_eq!(printer.verbosity(), Verbosity::Normal);
    }

    #[test]
    fn output_format_wide_preserves_verbosity() {
        let printer = Printer::with_format(Verbosity::Verbose, None, OutputFormat::Wide);
        assert_eq!(printer.verbosity(), Verbosity::Verbose);
    }

    // --- name_from_value additional fields ---

    #[test]
    fn name_from_value_url_field() {
        let val = serde_json::json!({"url": "https://example.com"});
        assert_eq!(
            name_from_value(&val),
            Some("https://example.com".to_string())
        );
    }

    #[test]
    fn name_from_value_resource_type_field() {
        let val = serde_json::json!({"resourceType": "MachineConfig"});
        assert_eq!(name_from_value(&val), Some("MachineConfig".to_string()));
    }
}
