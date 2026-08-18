//! Walk a `Doc` tree and dispatch each `Component` to the matching `Renderer`
//! method. Pure dispatcher — no layout, theming, or verbosity logic lives here.
//!
//! `Printer::render` is the force-human-render entry; `Printer::emit` routes
//! by `OutputFormat` and falls back to `render` for human formats.

use std::path::PathBuf;
use std::time::Duration;

use super::component::Component;
use super::doc::Doc;
use super::renderer::{Renderer, StatusFields, Table, Writer, finalize_subject};

pub(crate) fn render_doc(renderer: &Renderer, sink: &dyn Writer, doc: &Doc) {
    renderer.enter_doc();
    if let Some(h) = &doc.heading {
        renderer.render_heading(sink, h);
    }
    for child in &doc.children {
        render_component(renderer, sink, child, /*depth=*/ 0);
    }
    renderer.flush_kv_buffer(sink);
    renderer.exit_doc();
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
            label,
        } => {
            let target_pb: Option<PathBuf> = target.as_ref().map(PathBuf::from);
            // Sanitize caller-supplied subject ANSI BEFORE composing the
            // renderer-owned label SGR; matches `StatusBuilder::Drop`'s
            // boundary handling so both Doc and streaming paths stay
            // byte-identical.
            let subject_owned = finalize_subject(&renderer.theme, subject, None, label.as_ref());
            renderer.render_status(
                sink,
                depth,
                &StatusFields {
                    role: *role,
                    subject: &subject_owned,
                    detail: detail.as_deref(),
                    duration: duration_ms.map(|ms| Duration::from_millis(ms as u64)),
                    target: target_pb.as_deref(),
                    subject_style: None,
                    detail_style: None,
                },
            );
        }
        Component::Hint { text } => {
            renderer.render_hint(sink, depth, text);
        }
        Component::Note { text } => {
            renderer.render_note(sink, depth, text);
        }
        Component::CodeBlock { lines } => {
            renderer.render_code_block(sink, depth, lines);
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
    //! Plain-text snapshot tests (the default elsewhere in this crate)
    //! cannot catch a regression that drops `row_roles` mid-trip — the
    //! styling is invisible without colors enabled.

    use super::*;
    use crate::output::renderer::Renderer;
    use crate::output::{Role, Theme, Verbosity};
    use crate::test_helpers::EnvVarGuard;
    use std::sync::{Arc, Mutex};

    struct StringSink(Arc<Mutex<String>>);
    impl super::Writer for StringSink {
        fn write_line(&self, text: &str) {
            self.0.lock().unwrap().push_str(text);
            self.0.lock().unwrap().push('\n');
        }
    }

    #[test]
    #[serial_test::serial]
    fn doc_table_row_roles_reach_renderer_with_truecolor_escapes() {
        // `NO_COLOR` and `COLORTERM` are process-global and decide the
        // truecolor arm; both guards restore on unwind. The theme's own colour
        // stamp is what makes the render styled, so no flag is pinned here.
        let _no_color = EnvVarGuard::unset("NO_COLOR");
        let _colorterm = EnvVarGuard::set("COLORTERM", "truecolor");

        let theme = Theme::from_preset("dracula").with_colors(true);
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

        // raw-capture-ok: asserting on the raw truecolor SGR bytes themselves — captured_text would strip the ANSI this test exists to check
        let out = buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
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
    }
}
