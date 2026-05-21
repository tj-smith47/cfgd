use serde::Serialize;
use unicode_width::UnicodeWidthStr;

use super::{Renderer, Writer, role_glyph};
use crate::output::{Role, Verbosity};

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
}

impl Table {
    pub fn new(headers: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            headers: headers.into_iter().map(Into::into).collect(),
            rows: Vec::new(),
            row_roles: Vec::new(),
        }
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
        self.flush_pending_section_headers(w);
        let cols = t.headers.len();
        let mut widths = vec![0usize; cols];
        for (i, h) in t.headers.iter().enumerate() {
            widths[i] = UnicodeWidthStr::width(h.as_str());
        }
        for row in &t.rows {
            for (i, cell) in row.iter().enumerate().take(cols) {
                widths[i] = widths[i].max(UnicodeWidthStr::width(cell.as_str()));
            }
        }
        // Header row. Pad by the display-width deficit so CJK / emoji / accented
        // cells line up with ASCII neighbours — `format!("{:<w$}", ...)` pads by
        // char count, which over-pads multi-byte and under-pads zero-width.
        let header_line: String = t
            .headers
            .iter()
            .enumerate()
            .map(|(i, h)| {
                let pad = widths[i].saturating_sub(UnicodeWidthStr::width(h.as_str()));
                format!("{h}{}", " ".repeat(pad))
            })
            .collect::<Vec<_>>()
            .join("  ");
        let styled = self.theme.header.apply_to(&header_line).to_string();
        self.write_line(w, depth, &styled);
        // Separator
        let sep: String = widths
            .iter()
            .map(|w| "─".repeat(*w))
            .collect::<Vec<_>>()
            .join("──");
        let dim = self.theme.muted.apply_to(&sep).to_string();
        self.write_line(w, depth, &dim);
        // Data rows. Padding runs against the plain string so widths stay
        // honest; per-cell roles re-style the padded cell post-hoc.
        let empty_roles: Vec<Option<Role>> = Vec::new();
        for (row_idx, row) in t.rows.iter().enumerate() {
            let roles_for_row = t.row_roles.get(row_idx).unwrap_or(&empty_roles);
            let line: String = row
                .iter()
                .enumerate()
                .take(cols)
                .map(|(i, cell)| {
                    let pad = widths[i].saturating_sub(UnicodeWidthStr::width(cell.as_str()));
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
            self.write_line(w, depth, &line);
        }
        self.mark_top_level_blank_if_at_root();
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
        assert!(buf.lock().unwrap().is_empty());
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
        let out = buf.lock().unwrap();
        assert!(out.contains("Name"));
        assert!(out.contains("Age"));
        assert!(out.contains("─"));
        assert!(out.contains("alice"));
        assert!(out.contains("bob"));
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
        let out = crate::output::strip_ansi(&buf.lock().unwrap().clone());
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
        let out = crate::output::strip_ansi(&buf.lock().unwrap().clone());
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
        let out = crate::output::strip_ansi(&buf.lock().unwrap().clone());
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

    #[test]
    fn table_does_not_panic_on_tab_or_control_chars_in_cells() {
        // unicode_width::width treats \t and other C0 control codes as 0
        // display cells. Lock in the current "tab/control = 0 width" outcome:
        // the renderer must complete without panic or NaN widths, both rows
        // must appear in the output, and the second-column value columns must
        // align by display width across rows (the tab-bearing row's "0-width
        // tab" + "b" column ends up shorter than "plain", padding makes up
        // the difference).
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Normal);
        let t = Table::new(["Key", "Value"])
            .row(["a\tb\x0Bc", "ok"])
            .row(["plain", "yep"]);
        r.render_table(&sink, 0, &t);
        let out = crate::output::strip_ansi(&buf.lock().unwrap().clone());
        assert!(out.contains("ok"), "missing ok row: {out:?}");
        assert!(out.contains("yep"), "missing yep row: {out:?}");
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
