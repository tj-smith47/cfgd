//! Structured-output bridge for `Printer::emit`.
//!
//! Routes a `Doc` by `OutputFormat`:
//! - `Json` → pretty-printed JSON to stdout.
//! - `Yaml` → YAML to stdout (with leading `---\n` stripped).
//! - `Name` → name-field per item to stdout.
//! - `Jsonpath(expr)` → kubectl-style jsonpath result to stdout.
//! - `Template(str)` / `TemplateFile(path)` → tera-rendered to stdout.
//! - `Table` / `Wide` → returns `false`; caller falls back to `render_doc`.
//!
//! The four jsonpath helpers (`apply_jsonpath`, `walk_jsonpath`,
//! `split_jsonpath_segment`, `format_jsonpath_result`) are copied verbatim
//! from the legacy `output/structured.rs` — they depend only on
//! `serde_json` and are already battle-tested.

use super::OutputFormat;
use super::doc::Doc;
use super::renderer::Writer;

/// Apply a kubectl-compatible jsonpath expression to a JSON value.
///
/// Supports:
/// - `{.field.nested}` — dot-path into objects
/// - `{.items[0]}` — array index
/// - `{.items[*].name}` — wildcard collect
/// - `{.items[0:3]}` — array slice
///
/// Scalars return as raw text, objects/arrays as JSON.
pub(crate) fn apply_jsonpath(value: &serde_json::Value, expr: &str) -> String {
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
pub(crate) fn name_from_value(value: &serde_json::Value) -> Option<String> {
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

pub(crate) fn walk_jsonpath<'a>(
    value: &'a serde_json::Value,
    path: &str,
) -> Vec<&'a serde_json::Value> {
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
pub(crate) fn split_jsonpath_segment(path: &str) -> (&str, &str) {
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
pub(crate) fn format_jsonpath_result(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => serde_json::to_string_pretty(value).unwrap_or_default(),
    }
}

/// Route a `Doc` to `sink_stdout` per `format`. Returns `true` when the format
/// was handled (structured); returns `false` for `Table | Wide` so the caller
/// can fall back to the human renderer.
pub(crate) fn emit_structured(sink_stdout: &dyn Writer, doc: &Doc, format: &OutputFormat) -> bool {
    match format {
        OutputFormat::Table | OutputFormat::Wide => false,
        OutputFormat::Json => {
            let v = doc.data_or_self_json();
            let text = serde_json::to_string_pretty(&v).unwrap_or_default();
            sink_stdout.write_line(&text);
            true
        }
        OutputFormat::Yaml => {
            let v = doc.data_or_self_json();
            let yaml = serde_yaml::to_string(&v).unwrap_or_default();
            let trimmed = yaml.strip_prefix("---\n").unwrap_or(&yaml);
            sink_stdout.write_line(trimmed.trim_end());
            true
        }
        OutputFormat::Name => {
            let v = doc.data_or_self_json();
            match v {
                serde_json::Value::Array(arr) => {
                    for item in arr {
                        if let Some(name) = name_from_value(&item) {
                            sink_stdout.write_line(&name);
                        }
                    }
                }
                obj => {
                    if let Some(name) = name_from_value(&obj) {
                        sink_stdout.write_line(&name);
                    }
                }
            }
            true
        }
        OutputFormat::Jsonpath(expr) => {
            let v = doc.data_or_self_json();
            sink_stdout.write_line(&apply_jsonpath(&v, expr));
            true
        }
        OutputFormat::Template(tmpl) => {
            render_template_to(sink_stdout, doc, tmpl);
            true
        }
        OutputFormat::TemplateFile(path) => {
            let path = crate::expand_tilde(path);
            match std::fs::read_to_string(&path) {
                Ok(tmpl) => render_template_to(sink_stdout, doc, &tmpl),
                Err(e) => {
                    sink_stdout.write_line(&format!(
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

fn render_template_to(sink_stdout: &dyn Writer, doc: &Doc, template: &str) {
    let v = doc.data_or_self_json();
    let mut tera = tera::Tera::default();
    let tmpl_name = "__inline__";
    if let Err(e) = tera.add_raw_template(tmpl_name, template) {
        sink_stdout.write_line(&format!("invalid template: {e}"));
        return;
    }
    match v {
        serde_json::Value::Array(arr) => {
            for item in arr {
                match tera::Context::from_value(item.clone()) {
                    Ok(ctx) => match tera.render(tmpl_name, &ctx) {
                        Ok(rendered) => sink_stdout.write_line(&rendered),
                        Err(e) => sink_stdout.write_line(&format!("template render error: {e}")),
                    },
                    Err(e) => sink_stdout.write_line(&format!("template context error: {e}")),
                }
            }
        }
        other => match tera::Context::from_value(other) {
            Ok(ctx) => match tera.render(tmpl_name, &ctx) {
                Ok(rendered) => sink_stdout.write_line(&rendered),
                Err(e) => sink_stdout.write_line(&format!("template render error: {e}")),
            },
            Err(e) => sink_stdout.write_line(&format!("template context error: {e}")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::renderer::StringSink;
    use std::sync::{Arc, Mutex};

    fn capture() -> (Arc<Mutex<String>>, StringSink) {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        (buf, sink)
    }

    fn take(buf: &Arc<Mutex<String>>) -> String {
        buf.lock().unwrap().clone()
    }

    fn fixture() -> serde_json::Value {
        serde_json::json!({
            "items": [
                {"name": "alpha", "value": 1},
                {"name": "beta", "value": 2},
                {"name": "gamma", "value": 3},
            ],
            "nested": {"inner": {"field": "deep"}},
            "applyId": 42,
        })
    }

    // ---- apply_jsonpath -----------------------------------------------------

    #[test]
    fn apply_jsonpath_strips_brace_and_dot_prefix() {
        assert_eq!(apply_jsonpath(&fixture(), "{.applyId}"), "42");
    }

    #[test]
    fn apply_jsonpath_traverses_nested_objects() {
        assert_eq!(apply_jsonpath(&fixture(), ".nested.inner.field"), "deep");
    }

    #[test]
    fn apply_jsonpath_indexes_into_array() {
        assert_eq!(apply_jsonpath(&fixture(), ".items[0].name"), "alpha");
    }

    #[test]
    fn apply_jsonpath_wildcards_collect_all_elements() {
        assert_eq!(
            apply_jsonpath(&fixture(), ".items[*].name"),
            "alpha\nbeta\ngamma"
        );
    }

    #[test]
    fn apply_jsonpath_slices_array_range() {
        let v = fixture();
        let items = v["items"].as_array().unwrap();
        let expected = format!(
            "{}\n{}",
            serde_json::to_string_pretty(&items[0]).unwrap(),
            serde_json::to_string_pretty(&items[1]).unwrap(),
        );
        assert_eq!(apply_jsonpath(&v, ".items[0:2]"), expected);
    }

    #[test]
    fn apply_jsonpath_slice_with_empty_range_returns_empty() {
        assert_eq!(apply_jsonpath(&fixture(), ".items[3:3]"), "");
    }

    #[test]
    fn apply_jsonpath_slice_start_past_end_returns_empty() {
        assert_eq!(apply_jsonpath(&fixture(), ".items[10:20]"), "");
    }

    #[test]
    fn apply_jsonpath_returns_empty_for_missing_key() {
        assert_eq!(apply_jsonpath(&fixture(), ".nonexistent"), "");
    }

    #[test]
    fn apply_jsonpath_returns_empty_for_invalid_array_index() {
        assert_eq!(apply_jsonpath(&fixture(), ".items[99].name"), "");
    }

    #[test]
    fn apply_jsonpath_returns_empty_for_non_numeric_index() {
        assert_eq!(apply_jsonpath(&fixture(), ".items[abc].name"), "");
    }

    #[test]
    fn apply_jsonpath_empty_expr_returns_root_formatted() {
        let v = fixture();
        let expected = serde_json::to_string_pretty(&v).unwrap();
        assert_eq!(apply_jsonpath(&v, ""), expected);
    }

    #[test]
    fn apply_jsonpath_braces_optional() {
        assert_eq!(apply_jsonpath(&fixture(), "applyId"), "42");
    }

    // ---- name_from_value ----------------------------------------------------

    #[test]
    fn name_from_value_picks_name_first() {
        let v = serde_json::json!({"name": "X", "context": "Y"});
        assert_eq!(name_from_value(&v), Some("X".to_string()));
    }

    #[test]
    fn name_from_value_falls_back_to_context_when_no_name() {
        let v = serde_json::json!({"context": "ctx"});
        assert_eq!(name_from_value(&v), Some("ctx".to_string()));
    }

    #[test]
    fn name_from_value_falls_back_to_phase() {
        let v = serde_json::json!({"phase": "apply"});
        assert_eq!(name_from_value(&v), Some("apply".to_string()));
    }

    #[test]
    fn name_from_value_falls_back_to_resource_type() {
        let v = serde_json::json!({"resourceType": "Module"});
        assert_eq!(name_from_value(&v), Some("Module".to_string()));
    }

    #[test]
    fn name_from_value_falls_back_to_url() {
        let v = serde_json::json!({"url": "https://x"});
        assert_eq!(name_from_value(&v), Some("https://x".to_string()));
    }

    #[test]
    fn name_from_value_falls_back_to_apply_id_numeric() {
        let v = serde_json::json!({"applyId": 7});
        assert_eq!(name_from_value(&v), Some("7".to_string()));
    }

    #[test]
    fn name_from_value_returns_none_when_no_known_keys() {
        let v = serde_json::json!({"unrelated": "x"});
        assert_eq!(name_from_value(&v), None);
    }

    // ---- emit_structured ----------------------------------------------------

    fn doc_with(value: serde_json::Value) -> Doc {
        Doc::new().with_data(value)
    }

    #[test]
    fn emit_structured_table_returns_false_and_writes_nothing() {
        let (buf, sink) = capture();
        let doc = doc_with(serde_json::json!({"name": "x"}));
        let handled = emit_structured(&sink, &doc, &OutputFormat::Table);
        assert!(!handled);
        assert_eq!(take(&buf), "");
    }

    #[test]
    fn emit_structured_wide_returns_false_and_writes_nothing() {
        let (buf, sink) = capture();
        let doc = doc_with(serde_json::json!({"name": "x"}));
        let handled = emit_structured(&sink, &doc, &OutputFormat::Wide);
        assert!(!handled);
        assert_eq!(take(&buf), "");
    }

    #[test]
    fn emit_structured_json_writes_pretty_json() {
        let (buf, sink) = capture();
        let payload = serde_json::json!({"name": "x", "n": 1});
        let doc = doc_with(payload.clone());
        let handled = emit_structured(&sink, &doc, &OutputFormat::Json);
        assert!(handled);
        let expected = format!("{}\n", serde_json::to_string_pretty(&payload).unwrap());
        assert_eq!(take(&buf), expected);
    }

    #[test]
    fn emit_structured_yaml_strips_leading_doc_marker() {
        let (buf, sink) = capture();
        let payload = serde_json::json!({"name": "x", "n": 1});
        let doc = doc_with(payload.clone());
        let handled = emit_structured(&sink, &doc, &OutputFormat::Yaml);
        assert!(handled);
        let yaml_raw = serde_yaml::to_string(&payload).unwrap();
        let expected_body = yaml_raw
            .strip_prefix("---\n")
            .unwrap_or(&yaml_raw)
            .trim_end();
        let captured = take(&buf);
        assert!(
            !captured.starts_with("---\n"),
            "expected leading doc-marker stripped, got: {captured:?}"
        );
        assert_eq!(captured, format!("{expected_body}\n"));
    }

    #[test]
    fn emit_structured_name_writes_name_for_object() {
        let (buf, sink) = capture();
        let doc = doc_with(serde_json::json!({"name": "x"}));
        let handled = emit_structured(&sink, &doc, &OutputFormat::Name);
        assert!(handled);
        assert_eq!(take(&buf), "x\n");
    }

    #[test]
    fn emit_structured_name_writes_name_per_element_for_array() {
        let (buf, sink) = capture();
        let doc = doc_with(serde_json::json!([
            {"name": "alpha"},
            {"name": "beta"},
        ]));
        let handled = emit_structured(&sink, &doc, &OutputFormat::Name);
        assert!(handled);
        assert_eq!(take(&buf), "alpha\nbeta\n");
    }

    #[test]
    fn emit_structured_name_skips_elements_with_no_known_keys() {
        let (buf, sink) = capture();
        let doc = doc_with(serde_json::json!([
            {"name": "alpha"},
            {"unrelated": "skipped"},
            {"name": "gamma"},
        ]));
        let handled = emit_structured(&sink, &doc, &OutputFormat::Name);
        assert!(handled);
        assert_eq!(take(&buf), "alpha\ngamma\n");
    }

    #[test]
    fn emit_structured_jsonpath_writes_result_of_apply_jsonpath() {
        let (buf, sink) = capture();
        let doc = doc_with(serde_json::json!({"name": "x"}));
        let handled = emit_structured(&sink, &doc, &OutputFormat::Jsonpath("{.name}".into()));
        assert!(handled);
        assert_eq!(take(&buf), "x\n");
    }

    #[test]
    fn emit_structured_template_renders_inline_template() {
        let (buf, sink) = capture();
        let doc = doc_with(serde_json::json!({"name": "alpha"}));
        let handled = emit_structured(
            &sink,
            &doc,
            &OutputFormat::Template("name={{ name }}".into()),
        );
        assert!(handled);
        assert_eq!(take(&buf), "name=alpha\n");
    }

    #[test]
    fn emit_structured_template_renders_per_array_element() {
        let (buf, sink) = capture();
        let doc = doc_with(serde_json::json!([
            {"name": "alpha"},
            {"name": "beta"},
        ]));
        let handled = emit_structured(&sink, &doc, &OutputFormat::Template("{{ name }}".into()));
        assert!(handled);
        assert_eq!(take(&buf), "alpha\nbeta\n");
    }

    #[test]
    fn emit_structured_template_invalid_template_writes_error_line() {
        let (buf, sink) = capture();
        let doc = doc_with(serde_json::json!({"name": "x"}));
        let handled = emit_structured(&sink, &doc, &OutputFormat::Template("{{ ".into()));
        assert!(handled);
        let captured = take(&buf);
        assert!(
            captured.starts_with("invalid template: "),
            "expected 'invalid template:' prefix, got: {captured:?}"
        );
        assert_eq!(
            captured.lines().count(),
            1,
            "expected exactly one captured line"
        );
    }

    #[test]
    fn emit_structured_template_render_error_writes_error_line() {
        let (buf, sink) = capture();
        let doc = doc_with(serde_json::json!({"foo": "bar"}));
        let handled = emit_structured(
            &sink,
            &doc,
            &OutputFormat::Template("{{ foo | NONEXISTENT }}".into()),
        );
        assert!(handled);
        let captured = take(&buf);
        assert!(
            captured.starts_with("template render error: "),
            "expected 'template render error:' prefix, got: {captured:?}"
        );
        assert_eq!(
            captured.lines().count(),
            1,
            "expected exactly one captured line"
        );
    }

    #[test]
    fn emit_structured_template_file_missing_writes_error_line() {
        let (buf, sink) = capture();
        let doc = doc_with(serde_json::json!({"name": "x"}));
        let missing = std::path::PathBuf::from("/nonexistent/cfgd-structured-test/path.tmpl");
        let handled = emit_structured(&sink, &doc, &OutputFormat::TemplateFile(missing));
        assert!(handled);
        let captured = take(&buf);
        assert!(
            captured.starts_with("failed to read template file"),
            "expected 'failed to read template file' prefix, got: {captured:?}"
        );
    }

    #[test]
    fn emit_structured_template_file_reads_and_renders() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("greeting.tera");
        std::fs::write(&path, "hello {{ name }}").unwrap();
        let (buf, sink) = capture();
        let doc = doc_with(serde_json::json!({"name": "alpha"}));
        let handled = emit_structured(&sink, &doc, &OutputFormat::TemplateFile(path));
        assert!(handled);
        assert_eq!(take(&buf), "hello alpha\n");
    }
}
