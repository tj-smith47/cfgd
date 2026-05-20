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
        Component::Table { headers, rows } => {
            let t = Table {
                headers: headers.clone(),
                rows: rows.clone(),
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
