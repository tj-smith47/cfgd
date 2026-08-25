use serde::Serialize;
use unicode_width::UnicodeWidthStr;

use super::{Renderer, Writer, role_glyph, wrap};
use crate::output::{Role, Verbosity, cursor_safe};

/// Narrower than this a capped column carries almost nothing but its
/// ellipsis, so shrinking stops here and a terminal too narrow to hold every
/// column at the floor is left alone — the same stance `wrap::MIN_WRAP_WIDTH`
/// takes for a status line.
const MIN_COL_WIDTH: usize = 8;

/// Shrink `widths` until the row they lay out (columns plus the two-space
/// gaps between them) fits `budget`, always taking columns from the widest
/// one — the offending column pays, and every other column keeps its natural
/// width, which is the kubectl/docker-style degrade. No column drops below
/// [`MIN_COL_WIDTH`]; if the floors themselves overflow, what is left is left.
fn fit_widths(widths: &mut [usize], budget: usize) {
    if widths.is_empty() {
        return;
    }
    let gaps = 2 * (widths.len() - 1);
    loop {
        let total: usize = widths.iter().sum::<usize>() + gaps;
        if total <= budget {
            return;
        }
        let overflow = total - budget;
        let widest = (0..widths.len()).max_by_key(|&i| widths[i]).unwrap_or(0);
        if widths[widest] <= MIN_COL_WIDTH {
            return;
        }
        // Come down no further than the runner-up: past it a DIFFERENT column
        // is the widest and should pay the remainder instead.
        let runner_up = widths
            .iter()
            .enumerate()
            .filter(|&(i, _)| i != widest)
            .map(|(_, w)| *w)
            .max()
            .unwrap_or(0);
        let target = widths[widest]
            .saturating_sub(overflow)
            .max(runner_up)
            .max(MIN_COL_WIDTH);
        // A tie with the runner-up would leave the width unchanged; one column
        // must still shrink or nothing ever converges.
        widths[widest] = target.min(widths[widest] - 1);
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Table {
    pub(crate) headers: Vec<String>,
    pub(crate) rows: Vec<Vec<String>>,
    /// Per-cell role tags, parallel to `rows`. Each inner vec is the same
    /// length as its row's cell count, with `Some(role)` for cells that
    /// should be re-styled by the renderer after width-aware padding.
    /// Width calculation still uses the plain string in `rows`, so ANSI
    /// escapes from styling never inflate column widths.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) row_roles: Vec<Vec<Option<Role>>>,
    /// Whether a cell too wide for its column WRAPS onto continuation lines
    /// instead of being truncated with `…`. Display-only, like
    /// `Component::Section`'s `owner`: a `-o json` reader sees the same rows
    /// either way.
    #[serde(skip)]
    pub(crate) wrap_cells: bool,
}

impl Table {
    pub fn new(headers: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            headers: headers.into_iter().map(Into::into).collect(),
            rows: Vec::new(),
            row_roles: Vec::new(),
            wrap_cells: false,
        }
    }

    /// Wrap a cell onto continuation lines rather than truncating it, when the
    /// terminal cannot hold the table at its natural widths.
    ///
    /// For a table whose cells carry a LIST the reader has to act on — the
    /// package names one manager installed — where the tail `…` eats is data
    /// lost rather than a detail some wider view still offers.
    pub fn wrapping(mut self) -> Self {
        self.wrap_cells = true;
        self
    }

    pub fn row(mut self, row: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let cells: Vec<String> = row.into_iter().map(Into::into).collect();
        self.row_roles.push(vec![None; cells.len()]);
        self.rows.push(cells);
        self
    }

    /// Like `row` but each cell carries an optional `Role` whose style is
    /// applied after the row is padded to the column widths. Use this for
    /// columns where the value (`"pending"`, `"remote"`, `"merged"`) carries
    /// the semantic, and color amplifies it rather than replacing the text.
    pub fn row_styled<I, S>(mut self, row: I) -> Self
    where
        I: IntoIterator<Item = (S, Option<Role>)>,
        S: Into<String>,
    {
        let mut cells = Vec::new();
        let mut roles = Vec::new();
        for (value, role) in row {
            cells.push(value.into());
            roles.push(role);
        }
        self.rows.push(cells);
        self.row_roles.push(roles);
        self
    }
}

impl Renderer {
    pub fn render_table(&self, w: &dyn Writer, depth: usize, t: &Table) {
        if self.verbosity == Verbosity::Quiet || t.headers.is_empty() {
            return;
        }
        let cols = t.headers.len();
        // Headers and cells name things cfgd did not author — a module's alias
        // command, a remote source's description — and are folded BEFORE the
        // column widths are taken, never after: a width measured over a `\t`
        // or a `\r` describes text that is no longer what renders, and
        // mis-sizes every column after it.
        let headers: Vec<String> = t.headers.iter().map(|h| cursor_safe(h)).collect();
        let rows: Vec<Vec<String>> = t
            .rows
            .iter()
            .map(|row| row.iter().map(|c| cursor_safe(c)).collect())
            .collect();
        let mut widths = vec![0usize; cols];
        for (i, h) in headers.iter().enumerate() {
            widths[i] = UnicodeWidthStr::width(h.as_str());
        }
        for row in &rows {
            for (i, cell) in row.iter().enumerate().take(cols) {
                widths[i] = widths[i].max(UnicodeWidthStr::width(cell.as_str()));
            }
        }
        // A grid wider than the terminal is not a grid: the terminal
        // hard-wraps the over-wide row to column 0 and the header under its
        // own rule. Cap the widest column(s) until the row fits and truncate
        // what a capped column holds with `…`, so every row stays one line in
        // consistent columns. Only a sink that hard-wraps asks (a golden's
        // capture buffer answers `None` and keeps its natural widths).
        if let Some(term_cols) = w.wrap_columns() {
            fit_widths(&mut widths, wrap::line_budget(term_cols, depth));
        }
        let clamped = |cell: &str, i: usize| wrap::clamp(cell, widths[i]);
        // Lay every row out BEFORE any padding is decided. A wrapping column
        // breaks at word boundaries, so the widest line it actually paints can
        // be far narrower than the width it was allotted — padding to the
        // allotment leaves that slack in front of every later column, which
        // reads as the last column being right-anchored rather than
        // left-packed two spaces after its neighbour.
        let empty_roles: Vec<Option<Role>> = Vec::new();
        let laid_out_rows: Vec<Vec<Vec<String>>> = rows
            .iter()
            .map(|row| {
                row.iter()
                    .enumerate()
                    .take(cols)
                    .map(|(i, cell)| {
                        if t.wrap_cells {
                            wrap::wrap_segment(cell, "", "", Some(widths[i]))
                        } else {
                            vec![clamped(cell, i)]
                        }
                    })
                    .collect()
            })
            .collect();
        let mut widths: Vec<usize> = headers
            .iter()
            .enumerate()
            .map(|(i, h)| UnicodeWidthStr::width(clamped(h, i).as_str()))
            .collect();
        for row in &laid_out_rows {
            for (i, lines) in row.iter().enumerate() {
                let widest = lines
                    .iter()
                    .map(|l| UnicodeWidthStr::width(l.as_str()))
                    .max()
                    .unwrap_or(0);
                widths[i] = widths[i].max(widest);
            }
        }
        // Header row. Pad by the display-width deficit so CJK / emoji / accented
        // cells line up with ASCII neighbours — `format!("{:<w$}", ...)` pads by
        // char count, which over-pads multi-byte and under-pads zero-width.
        // Each cell is styled on its own: styling the joined row instead
        // would wrap the "  " inter-column gap in the same SGR span, so a
        // reader selecting just the gap copies an empty styled run.
        let header_line: String = headers
            .iter()
            .enumerate()
            .map(|(i, h)| {
                let h = clamped(h, i);
                let pad = widths[i].saturating_sub(UnicodeWidthStr::width(h.as_str()));
                let padded = format!("{h}{}", " ".repeat(pad));
                self.theme.header.apply_to(padded).to_string()
            })
            .collect::<Vec<_>>()
            .join("  ");
        // One emission: header row, separator and every data row leave under a
        // single state-lock acquisition, so a live bar redraws once around the
        // whole table rather than once per row.
        let mut body = vec![header_line];
        // Separator
        let sep: String = widths
            .iter()
            .map(|w| "─".repeat(*w))
            .collect::<Vec<_>>()
            .join("──");
        body.push(self.theme.muted.apply_to(&sep).to_string());
        // Data rows. Padding runs against the plain string so widths stay
        // honest; per-cell roles re-style the padded cell post-hoc.
        for (row_idx, laid_out) in laid_out_rows.iter().enumerate() {
            let roles_for_row = t.row_roles.get(row_idx).unwrap_or(&empty_roles);
            // One physical line per cell normally; a wrapping table lays each
            // cell out over as many as its column needs, and the row is as
            // tall as its tallest cell.
            let height = laid_out.iter().map(Vec::len).max().unwrap_or(0);
            for physical in 0..height {
                let line: String = laid_out
                    .iter()
                    .enumerate()
                    .map(|(i, lines)| {
                        let cell = lines.get(physical).map(String::as_str).unwrap_or("");
                        let pad = widths[i].saturating_sub(UnicodeWidthStr::width(cell));
                        let padded = format!("{cell}{}", " ".repeat(pad));
                        match roles_for_row.get(i).and_then(|r| *r) {
                            Some(role) => {
                                let (_icon, style) = role_glyph(&self.theme, role);
                                style.apply_to(padded).to_string()
                            }
                            None => padded,
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("  ");
                body.push(line);
            }
        }
        self.emit_with(w, |e| {
            e.flush_section_headers();
            for line in &body {
                e.push_line(depth, line);
            }
            e.mark_top_level_group(super::TopGroup::Table);
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::super::StringSink;
    use super::*;
    use crate::output::Theme;

    #[test]
    fn empty_table_emits_nothing() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Normal);
        let t = Table::new(std::iter::empty::<String>());
        r.render_table(&sink, 0, &t);
        assert!(crate::test_helpers::captured_text(&buf).is_empty());
    }

    #[test]
    fn header_then_separator_then_rows() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Normal);
        let t = Table::new(["Name", "Age"])
            .row(["alice", "30"])
            .row(["bob", "25"]);
        r.render_table(&sink, 0, &t);
        let out = crate::test_helpers::captured_text(&buf);
        assert!(out.contains("Name"));
        assert!(out.contains("Age"));
        assert!(out.contains("─"));
        assert!(out.contains("alice"));
        assert!(out.contains("bob"));
    }

    /// The header row styles each cell on its own — the "  " gap
    /// between columns must stay outside any SGR span. Raw-captured (per
    /// testing.md) because the assertion is ABOUT the escapes: `captured_text`
    /// would strip exactly the bytes this test exists to check, and a bug
    /// that wraps the whole joined row in one span would pass a stripped-text
    /// comparison just as well as this one.
    #[test]
    #[serial_test::serial]
    fn table_header_styles_each_cell_leaving_gaps_unstyled() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let theme = Theme::from_preset("dracula").with_colors(true);
        let r = Renderer::new(theme.clone(), Verbosity::Normal);
        // Rows widen the "Age" column past the header's own width, so the
        // padding this test exists to prove lands inside the styled span
        // rather than the column happening to need none.
        let t = Table::new(["Name", "Age"]).row(["Bob", "1000"]);
        r.render_table(&sink, 0, &t);
        // raw-capture-ok: asserting the header row's SGR spans stop at each cell rather than spanning the inter-column gap — captured_text would strip the ANSI this test exists to check
        let raw = buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let expected = format!(
            "{}  {}",
            theme.header.apply_to("Name"),
            theme.header.apply_to("Age "),
        );
        let header_line = raw
            .lines()
            .next()
            .unwrap_or_else(|| panic!("no header line captured: {raw:?}"));
        assert_eq!(
            header_line, expected,
            "each header cell must carry its own SGR span, with a plain, \
             unstyled \"  \" gap between them: {header_line:?}"
        );
        assert!(
            expected.contains('\u{1b}'),
            "the fixture theme must actually emit SGR codes, or this test \
             proves nothing: {expected:?}"
        );
    }

    #[test]
    fn row_appends_keep_rows_and_roles_in_lockstep() {
        let t = Table::new(["a", "b"]).row(["1", "2"]).row(["3", "4"]);
        assert_eq!(t.rows.len(), t.row_roles.len());
        assert_eq!(t.rows[0].len(), t.row_roles[0].len());
        assert_eq!(t.rows[1].len(), t.row_roles[1].len());
    }

    #[test]
    fn row_styled_appends_keep_rows_and_roles_in_lockstep() {
        let t = Table::new(["a", "b"])
            .row(["plain", "plain"])
            .row_styled([("styled", Some(Role::Ok)), ("nostyle", None)]);
        assert_eq!(t.rows.len(), t.row_roles.len());
        assert_eq!(t.row_roles[1], vec![Some(Role::Ok), None]);
    }

    /// Returns the display-width prefix of `line` ending at the first byte
    /// occurrence of `needle`. Drives the column-alignment assertions for
    /// CJK / emoji rows where byte index ≠ display column.
    fn prefix_display_width(line: &str, needle: &str) -> usize {
        let idx = line
            .find(needle)
            .unwrap_or_else(|| panic!("needle {needle:?} missing in line {line:?}"));
        UnicodeWidthStr::width(&line[..idx])
    }

    #[test]
    fn table_aligns_cjk_cells_by_display_width() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Normal);
        let t = Table::new(["Name", "Score"])
            .row(["京都", "100"])
            .row(["Tokyo", "200"]);
        r.render_table(&sink, 0, &t);
        let out = crate::test_helpers::captured_text(&buf);
        let lines: Vec<&str> = out.lines().collect();
        let kyoto = lines
            .iter()
            .find(|l| l.contains("京都"))
            .expect("kyoto row missing");
        let tokyo = lines
            .iter()
            .find(|l| l.contains("Tokyo"))
            .expect("tokyo row missing");
        assert_eq!(
            prefix_display_width(kyoto, "100"),
            prefix_display_width(tokyo, "200"),
            "CJK and ASCII rows must align by display width.\nout:\n{out}"
        );
    }

    #[test]
    fn table_aligns_emoji_cells_by_display_width() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Normal);
        let t = Table::new(["Status", "Note"])
            .row(["✓", "ok"])
            .row(["⚠️", "warn"])
            .row(["complete", "yep"]);
        r.render_table(&sink, 0, &t);
        let out = crate::test_helpers::captured_text(&buf);
        let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
        let ok_line = lines
            .iter()
            .find(|l| l.contains(" ok"))
            .expect("ok row missing");
        let warn_line = lines
            .iter()
            .find(|l| l.contains("warn"))
            .expect("warn row missing");
        let yep_line = lines
            .iter()
            .find(|l| l.contains("yep"))
            .expect("yep row missing");
        let positions = [
            prefix_display_width(ok_line, "ok"),
            prefix_display_width(warn_line, "warn"),
            prefix_display_width(yep_line, "yep"),
        ];
        let first = positions[0];
        for (i, p) in positions.iter().enumerate() {
            assert_eq!(
                *p, first,
                "row {i} note column misaligned, got width {p}, expected {first}\nout:\n{out}"
            );
        }
    }

    #[test]
    fn table_aligns_cyrillic_cells_by_display_width() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Normal);
        let t = Table::new(["Name", "City"])
            .row(["Москва", "ru"])
            .row(["Paris", "fr"]);
        r.render_table(&sink, 0, &t);
        let out = crate::test_helpers::captured_text(&buf);
        let lines: Vec<&str> = out.lines().collect();
        let ru = lines
            .iter()
            .find(|l| l.contains("Москва"))
            .expect("ru row missing");
        let fr = lines
            .iter()
            .find(|l| l.contains("Paris"))
            .expect("fr row missing");
        assert_eq!(
            prefix_display_width(ru, "ru"),
            prefix_display_width(fr, "fr"),
            "Cyrillic and ASCII rows must align by display width.\nout:\n{out}"
        );
    }

    use super::super::NarrowSink;

    fn display_width(line: &str) -> usize {
        UnicodeWidthStr::width(line.trim_end())
    }

    /// The demo-terminal shape that broke the grid: a `cfgd status` Managed
    /// Resources row whose `module:packages:…` resource id is wider than the
    /// terminal. Left to the terminal it hard-wrapped the row to column 0 and
    /// the "Source" header under "Resource" with a doubled rule; capped, every
    /// row stays one line in consistent columns with the cut marked `…`.
    #[test]
    fn an_over_wide_cell_is_capped_into_the_grid_instead_of_wrapping_out_of_it() {
        let cols = 100;
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = NarrowSink(StringSink(buf.clone()), cols);
        let r = Renderer::new(Theme::default(), Verbosity::Normal);
        let packages: Vec<String> = (0..28).map(|i| format!("pkg-number-{i}")).collect();
        let wide = format!("nvim:packages:{}", packages.join(","));
        let t = Table::new(["Type", "Resource", "Source"])
            .row(["file", "~/.config/nvim", "module:nvim"])
            .row(["packages", wide.as_str(), "module:nvim"]);
        r.render_table(&sink, 0, &t);
        let out = crate::test_helpers::captured_text(&buf);
        let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();

        // One header line carrying every header, one rule, one line per row —
        // a dropped or terminal-wrapped row would change the count.
        assert_eq!(lines.len(), 4, "header, rule, two rows: {out:?}");
        assert!(
            lines[0].contains("Type")
                && lines[0].contains("Resource")
                && lines[0].contains("Source"),
            "every header stays on the one header line: {:?}",
            lines[0]
        );
        for line in &lines {
            assert!(
                display_width(line) <= cols,
                "a line wider than the terminal is a wrapped grid: {line:?}"
            );
        }
        let wide_row = lines
            .iter()
            .find(|l| l.contains("nvim:packages:"))
            .expect("the over-wide row must still render");
        assert!(
            wide_row.contains('…'),
            "the cap is marked, not silent: {wide_row:?}"
        );
        // The capped cell keeps its column: Source still lands at the same
        // display column on both data rows.
        let narrow_row = lines
            .iter()
            .find(|l| l.contains("~/.config/nvim"))
            .expect("the ordinary row must render");
        assert_eq!(
            prefix_display_width(wide_row, "module:nvim"),
            prefix_display_width(narrow_row, "module:nvim"),
            "the Source column must stay aligned across rows:\n{out}"
        );
    }

    /// The same over-wide row under [`Table::wrapping`]: a package list is
    /// what the reader acts on, so the tail is carried onto continuation lines
    /// under its own column instead of being cut with `…`. Every name the row
    /// holds is still readable, and the columns still line up.
    #[test]
    fn a_wrapping_table_carries_an_over_wide_cell_onto_continuation_lines() {
        let cols = 100;
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = NarrowSink(StringSink(buf.clone()), cols);
        let r = Renderer::new(Theme::default(), Verbosity::Normal);
        let packages: Vec<String> = (0..28).map(|i| format!("pkg-number-{i}")).collect();
        let wide = format!("apt: {}", packages.join(", "));
        let t = Table::new(["Type", "Owner", "Resource", "Source"])
            .row(["files", "module:nvim", "~/.config/nvim (6 files)", "local"])
            .row(["packages", "module:nvim", wide.as_str(), "local"])
            .wrapping();
        r.render_table(&sink, 0, &t);
        let out = crate::test_helpers::captured_text(&buf);
        let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();

        assert!(
            !out.contains('…'),
            "a wrapping table never truncates: {out:?}"
        );
        for name in &packages {
            assert!(
                out.contains(name.as_str()),
                "every package name survives the wrap, {name} did not: {out:?}"
            );
        }
        for line in &lines {
            assert!(
                display_width(line) <= cols,
                "a line wider than the terminal is a wrapped grid: {line:?}"
            );
        }
        // The continuation lines hang under the Resource column, so the row
        // still reads as one row: nothing lands in the Type or Owner columns.
        let first_wrapped = lines
            .iter()
            .position(|l| l.contains("apt: "))
            .expect("the wide row must render");
        let continuation = lines
            .get(first_wrapped + 1)
            .expect("the wide row must wrap onto a second line");
        let indent = UnicodeWidthStr::width(
            &continuation[..continuation.len() - continuation.trim_start().len()],
        );
        assert_eq!(
            indent,
            prefix_display_width(lines[0], "Resource"),
            "a continuation line starts at the Resource column:\n{out}"
        );
    }

    /// Every column is left-packed two spaces after the widest value its
    /// neighbour actually paints. A wrapping column breaks at word boundaries,
    /// so the lines it emits are usually narrower than the width it was
    /// allotted; padding to the allotment instead of to the painted width
    /// pushes the last column right by the unused remainder, and the reader
    /// sees a right-anchored `Source`.
    #[test]
    fn a_wrapped_column_is_sized_by_what_it_paints_not_by_its_allotment() {
        let cols = 100;
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = NarrowSink(StringSink(buf.clone()), cols);
        let r = Renderer::new(Theme::default(), Verbosity::Normal);
        let packages: Vec<String> = (0..28).map(|i| format!("pkg-number-{i}")).collect();
        let wide = format!("apt: {}", packages.join(", "));
        let t = Table::new(["Type", "Owner", "Resource", "Source"])
            .row(["file", "module:nvim", "~/.config/nvim (6 files)", "local"])
            .row(["package", "module:nvim", wide.as_str(), "local"])
            .wrapping();
        r.render_table(&sink, 0, &t);
        let out = crate::test_helpers::captured_text(&buf);
        let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();

        let resource_col = prefix_display_width(lines[0], "Resource");
        let source_col = prefix_display_width(lines[0], "Source");
        let widest_resource = lines
            .iter()
            .enumerate()
            // Index 1 is the `─` rule, whose columns are joined by `──` rather
            // than by the two-space gap this measurement splits on.
            .filter(|(i, _)| *i != 1)
            .map(|(_, l)| {
                let cell: String = l.chars().skip(resource_col).collect();
                let cell = cell.split("  ").next().unwrap_or_default().to_string();
                display_width(&cell)
            })
            .max()
            .unwrap_or(0);
        assert_eq!(
            source_col,
            resource_col + widest_resource + 2,
            "Source is left-packed two spaces after the widest painted Resource \
             value (widest {widest_resource}):\n{out}"
        );
    }

    /// A table that already fits its terminal renders exactly as it does on a
    /// non-wrapping sink: the cap only engages on overflow.
    #[test]
    fn a_table_that_fits_its_terminal_is_left_at_natural_widths() {
        let render_with = |sink: &dyn Writer, buf: &Arc<Mutex<String>>| {
            let r = Renderer::new(Theme::default(), Verbosity::Normal);
            let t = Table::new(["Name", "Age"])
                .row(["alice", "30"])
                .row(["bob", "25"]);
            r.render_table(sink, 0, &t);
            crate::test_helpers::captured_text(buf)
        };
        let wide_buf = Arc::new(Mutex::new(String::new()));
        let wide = render_with(&NarrowSink(StringSink(wide_buf.clone()), 100), &wide_buf);
        let plain_buf = Arc::new(Mutex::new(String::new()));
        let plain = render_with(&StringSink(plain_buf.clone()), &plain_buf);
        assert_eq!(wide, plain, "a fitting table must not be reshaped");
        assert!(!wide.contains('…'), "nothing to cap: {wide:?}");
    }

    /// A terminal too narrow to hold every column at the floor is left alone —
    /// the same stance `wrap_body` takes below `MIN_WRAP_WIDTH`. The grid
    /// still degrades no further than the floors.
    #[test]
    fn a_pathologically_narrow_terminal_stops_shrinking_at_the_floor() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = NarrowSink(StringSink(buf.clone()), 10);
        let r = Renderer::new(Theme::default(), Verbosity::Normal);
        let t = Table::new(["Resource", "Source"])
            .row(["something-quite-long-indeed", "another-long-value-here"]);
        r.render_table(&sink, 0, &t);
        let out = crate::test_helpers::captured_text(&buf);
        let row = out
            .lines()
            .find(|l| l.contains('…'))
            .expect("both columns capped to the floor");
        assert!(
            display_width(row) <= 2 * MIN_COL_WIDTH + 2,
            "no column shrinks past the floor, none grows either: {row:?}"
        );
    }

    #[test]
    fn fit_widths_takes_from_the_widest_and_leaves_the_rest_natural() {
        let mut widths = vec![8, 200, 11];
        fit_widths(&mut widths, 100);
        assert_eq!(widths[0], 8, "an innocent column keeps its width");
        assert_eq!(widths[2], 11, "an innocent column keeps its width");
        assert_eq!(
            widths.iter().sum::<usize>() + 4,
            100,
            "the offending column pays exactly the overflow: {widths:?}"
        );
    }

    #[test]
    fn fit_widths_levels_ties_instead_of_looping_forever() {
        let mut widths = vec![50, 50, 50];
        fit_widths(&mut widths, 60);
        assert!(
            widths.iter().sum::<usize>() + 4 <= 60,
            "equal columns level down together: {widths:?}"
        );
        assert!(
            widths.iter().all(|w| *w >= MIN_COL_WIDTH),
            "no column drops below the floor: {widths:?}"
        );
    }

    #[test]
    fn table_renders_control_chars_in_cells_visibly_and_still_aligns() {
        // A control character in a cell is folded to its visible `\xNN` form
        // before the column widths are taken, so it occupies the columns it
        // shows and the next column still lines up. Measuring first would size
        // the column against text that no longer renders — `unicode_width`
        // gives a raw `\t` zero cells, and the four the escape really takes
        // would push every later column out by four.
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Normal);
        let t = Table::new(["Key", "Value"])
            .row(["a\tb\x0Bc", "ok"])
            .row(["plain", "yep"]);
        r.render_table(&sink, 0, &t);
        let out = crate::test_helpers::captured_text(&buf);
        assert!(out.contains("ok"), "missing ok row: {out:?}");
        assert!(out.contains("yep"), "missing yep row: {out:?}");
        assert!(
            out.contains("a\\x09b\\x0bc"),
            "the cell's control characters must render as visible escapes: {out:?}"
        );
        let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
        let ok_line = lines
            .iter()
            .find(|l| l.contains("ok"))
            .expect("ok row missing");
        let yep_line = lines
            .iter()
            .find(|l| l.contains("yep"))
            .expect("yep row missing");
        assert_eq!(
            prefix_display_width(ok_line, "ok"),
            prefix_display_width(yep_line, "yep"),
            "tab/control-bearing row and plain row should align to the same display column"
        );
    }
}
