//! Walk a `Doc` tree and dispatch each `Component` to the matching `Renderer`
//! method. Pure dispatcher — no layout, theming, or verbosity logic lives here.
//!
//! `Printer::render` is the force-human-render entry; `Printer::emit` (T24)
//! will route by `OutputFormat` and fall back to `render` for human formats.

use std::path::PathBuf;
use std::time::Duration;

use super::component::Component;
use super::doc::Doc;
use super::renderer::{Renderer, StatusFields, Table, Writer};

pub(crate) fn render_doc(renderer: &Renderer, sink: &dyn Writer, doc: &Doc) {
    if let Some(h) = &doc.heading {
        renderer.render_heading(sink, h);
    }
    for child in &doc.children {
        render_component(renderer, sink, child, /*depth=*/ 0);
    }
    renderer.flush_kv_buffer(sink);
}

fn render_component(renderer: &Renderer, sink: &dyn Writer, c: &Component, depth: usize) {
    match c {
        Component::Heading { text } => {
            renderer.render_heading(sink, text);
        }
        Component::KvBlock { pairs } => {
            let pairs: Vec<(String, String)> = pairs
                .iter()
                .map(|p| (p.key.clone(), p.value.clone()))
                .collect();
            renderer.render_kv_block(sink, depth, &pairs);
        }
        Component::Bullet { text } => {
            renderer.render_bullet(sink, depth, text);
        }
        Component::Status {
            role,
            subject,
            detail,
            duration_ms,
            target,
        } => {
            let target_pb: Option<PathBuf> = target.as_ref().map(PathBuf::from);
            renderer.render_status(
                sink,
                depth,
                &StatusFields {
                    role: *role,
                    subject,
                    detail: detail.as_deref(),
                    duration: duration_ms.map(|ms| Duration::from_millis(ms as u64)),
                    target: target_pb.as_deref(),
                },
            );
        }
        Component::Hint { text } => {
            renderer.render_hint(sink, depth, text);
        }
        Component::Note { text } => {
            renderer.render_note(sink, depth, text);
        }
        Component::Table {
            headers,
            rows,
            row_roles,
        } => {
            let t = Table {
                headers: headers.clone(),
                rows: rows.clone(),
                row_roles: row_roles.clone(),
            };
            renderer.render_table(sink, depth, &t);
        }
        Component::Section {
            name,
            keep_when_empty,
            empty_state,
            children,
        } => {
            renderer.render_section_open(name, *keep_when_empty);
            if let Some(es) = empty_state {
                renderer.render_section_empty_state(es);
            }
            for child in children {
                render_component(renderer, sink, child, depth + 1);
            }
            renderer.render_section_close(sink);
        }
    }
}

#[cfg(test)]
mod row_roles_round_trip_tests {
    //! Anchor that `Table::row_styled` survives the `Doc::table` →
    //! `Component::Table` → `render_doc::render_component` →
    //! `Renderer::render_table` round trip with real ANSI escapes on output.
    //! Plain-text snapshots (default in this crate's other test buckets)
    //! cannot catch a regression that drops `row_roles` mid-trip — the
    //! styling is invisible without colors enabled.

    use super::*;
    use crate::output::renderer::Renderer;
    use crate::output::{Role, Theme, Verbosity};
    use std::sync::{Arc, Mutex};

    struct StringSink(Arc<Mutex<String>>);
    impl super::Writer for StringSink {
        fn write_line(&self, text: &str) {
            self.0.lock().unwrap().push_str(text);
            self.0.lock().unwrap().push('\n');
        }
    }

    #[test]
    fn doc_table_row_roles_reach_renderer_with_truecolor_escapes() {
        let _restore_no_color = std::env::var("NO_COLOR").ok();
        let _restore_colorterm = std::env::var("COLORTERM").ok();
        // SAFETY: setting env in a test process; restored in best-effort fashion
        // below. Single-threaded test enforced by serial_test in callers that need it.
        unsafe {
            std::env::set_var("COLORTERM", "truecolor");
            std::env::remove_var("NO_COLOR");
        }
        let was_enabled = console::colors_enabled();
        console::set_colors_enabled(true);

        let theme = Theme::from_preset("dracula");
        let renderer = Renderer::new(theme, Verbosity::Normal);
        let buf: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());

        let t = Table::new(["Source", "Status"])
            .row_styled([("local".to_string(), None), ("installed".to_string(), None)])
            .row_styled([
                ("remote".to_string(), Some(Role::Secondary)),
                ("pending".to_string(), Some(Role::Accent)),
            ]);
        let doc = Doc::new().table(t);
        render_doc(&renderer, &sink, &doc);

        let out = buf.lock().unwrap().clone();
        let dracula_pink = "\x1b[38;2;255;121;198m";
        let dracula_orange = "\x1b[38;2;255;184;108m";
        assert!(
            out.contains(dracula_pink),
            "secondary (pink) must reach renderer; got:\n{out:?}"
        );
        assert!(
            out.contains(dracula_orange),
            "accent (orange) must reach renderer; got:\n{out:?}"
        );

        console::set_colors_enabled(was_enabled);
        unsafe {
            match _restore_no_color {
                Some(v) => std::env::set_var("NO_COLOR", v),
                None => std::env::remove_var("NO_COLOR"),
            }
            match _restore_colorterm {
                Some(v) => std::env::set_var("COLORTERM", v),
                None => std::env::remove_var("COLORTERM"),
            }
        }
    }
}
