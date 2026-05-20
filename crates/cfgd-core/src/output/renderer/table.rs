use serde::Serialize;

use super::{Renderer, Writer, role_glyph};
use crate::output::{Role, Verbosity};

#[derive(Debug, Clone, Serialize)]
pub struct Table {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    /// Per-cell role tags, parallel to `rows`. Each inner vec is the same
    /// length as its row's cell count, with `Some(role)` for cells that
    /// should be re-styled by the renderer after width-aware padding.
    /// Width calculation still uses the plain string in `rows`, so ANSI
    /// escapes from styling never inflate column widths.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub row_roles: Vec<Vec<Option<Role>>>,
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
            widths[i] = h.len();
        }
        for row in &t.rows {
            for (i, cell) in row.iter().enumerate().take(cols) {
                widths[i] = widths[i].max(cell.len());
            }
        }
        // Header row
        let header_line: String = t
            .headers
            .iter()
            .enumerate()
            .map(|(i, h)| format!("{:<width$}", h, width = widths[i]))
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
                    let padded = format!("{:<width$}", cell, width = widths[i]);
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
}
