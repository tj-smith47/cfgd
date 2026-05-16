use serde::Serialize;

use super::{Renderer, Writer};
use crate::output_v2::Verbosity;

#[derive(Debug, Clone, Serialize)]
pub struct Table {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

impl Table {
    pub fn new(headers: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            headers: headers.into_iter().map(Into::into).collect(),
            rows: Vec::new(),
        }
    }

    pub fn row(mut self, row: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.rows.push(row.into_iter().map(Into::into).collect());
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
        // Data rows
        for row in &t.rows {
            let line: String = row
                .iter()
                .enumerate()
                .take(cols)
                .map(|(i, cell)| format!("{:<width$}", cell, width = widths[i]))
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
    use crate::output_v2::Theme;

    #[test]
    fn empty_table_emits_nothing() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Normal);
        let t = Table {
            headers: vec![],
            rows: vec![],
        };
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
