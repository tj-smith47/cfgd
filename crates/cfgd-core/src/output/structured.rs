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
use crate::PathDisplayExt;

/// Validate that a jsonpath expression is structurally well-formed without
/// applying it to any data. Returns `Err(message)` for malformed input so the
/// CLI value-parser (`OutputFormatArg::from_str`) can reject it before dispatch,
/// turning a would-be runtime panic into a clean clap usage error.
///
/// Checks (bracket defects reported before wrapper imbalance, so the diagnostic
/// names the real problem): brackets are balanced, never nested, never empty,
/// and every `[` is closed by `]` before the next `[`, before a `.`, and before
/// end-of-expression; the optional `{ }` wrapper is balanced.
pub fn validate_jsonpath_expr(expr: &str) -> Result<(), String> {
    let expr = expr.trim();
    // Strip an optional leading `{` and trailing `}` independently. A bracket
    // defect in the body is the more useful diagnostic than a wrapper imbalance
    // (e.g. `{.items[` is an unterminated `[`, not a wrapper problem), so scan
    // the body for bracket errors FIRST and only report an unbalanced `{ }`
    // wrapper when the body is otherwise well-formed.
    let has_open = expr.starts_with('{');
    let has_close = expr.ends_with('}');
    let body_start = if has_open { 1 } else { 0 };
    // Guard the slice: a lone `{` (len 1) must not underflow `len - 1`.
    let body_end = if has_close && expr.len() > body_start {
        expr.len() - 1
    } else {
        expr.len()
    };
    let inner = &expr[body_start..body_end];

    let mut in_bracket = false;
    let mut bracket_has_content = false;
    for c in inner.chars() {
        match c {
            '{' | '}' => {
                return Err(format!(
                    "malformed jsonpath '{expr}': stray '{c}' inside expression"
                ));
            }
            '[' => {
                if in_bracket {
                    return Err(format!("malformed jsonpath '{expr}': nested '['"));
                }
                in_bracket = true;
                bracket_has_content = false;
            }
            ']' => {
                if !in_bracket {
                    return Err(format!(
                        "malformed jsonpath '{expr}': ']' without matching '['"
                    ));
                }
                if !bracket_has_content {
                    return Err(format!("malformed jsonpath '{expr}': empty '[]'"));
                }
                in_bracket = false;
            }
            '.' if in_bracket => {
                return Err(format!(
                    "malformed jsonpath '{expr}': unterminated '[' before '.'"
                ));
            }
            _ => {
                if in_bracket {
                    bracket_has_content = true;
                }
            }
        }
    }
    if in_bracket {
        return Err(format!(
            "malformed jsonpath '{expr}': unterminated '[' (missing ']')"
        ));
    }
    // Brackets are well-formed; a half-open `{ }` wrapper is the only remaining
    // defect to report.
    if has_open != has_close {
        return Err(format!(
            "malformed jsonpath '{expr}': unbalanced {{ }} wrapper"
        ));
    }
    Ok(())
}

/// Apply a kubectl-compatible jsonpath expression to a JSON value.
///
/// Supports:
/// - `{.field.nested}` — dot-path into objects
/// - `{[0].name}` — index into a **bare** top-level array (what list commands
///   emit: `cfgd profile list -o json` is `[ {...}, {...} ]`, not an
///   `{"items": [...]}` envelope)
/// - `{.field[0]}` — index into an array nested under a key
/// - `{[*].name}` / `{.field[*].name}` — wildcard collect
/// - `{[0:3]}` / `{.field[0:3]}` — array slice
///
/// Note the bare-array model: against a list payload the working expression is
/// `{[0].name}`, NOT the kubectl KRM-envelope `{.items[0].name}` (that form
/// returns empty because the payload has no `items` key). The `.items[...]`
/// form applies only when a command's payload genuinely is an object with an
/// `items` field.
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
        // The segment must end in `]` for a well-formed bracket expression.
        // Without it the byte range `bracket_pos+1..len-1` would be inverted
        // and slicing would panic — a malformed expr must yield no results, not
        // a crash. (Belt-and-suspenders alongside `validate_jsonpath_expr`,
        // which rejects this at parse time.)
        let bracket_expr = match segment.strip_suffix(']') {
            Some(inner) => &inner[bracket_pos + 1..], // strip [ and trailing ]
            None => return vec![],
        };

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

/// Render a filesystem read failure into a clean, user-facing reason without the
/// platform-specific `(os error N)` tail that `io::Error`'s `Display` appends.
fn clean_io_reason(e: &std::io::Error) -> String {
    use std::io::ErrorKind;
    match e.kind() {
        ErrorKind::NotFound => "no such file".to_string(),
        ErrorKind::PermissionDenied => "permission denied".to_string(),
        // `IsADirectory` is stable since Rust 1.83; matching it avoids the
        // "Is a directory (os error 21)" leak when the path is a directory.
        ErrorKind::IsADirectory => "is a directory".to_string(),
        // Any other kind: use the kind's clean description, never the raw
        // `Display` (which carries the `(os error N)` suffix).
        other => other.to_string(),
    }
}

/// Wrap a top-level JSON array in a Kubernetes-style List envelope
/// (apiVersion / kind: List / items). Reached only under `-o json` / `-o yaml`
/// when `--list-envelope` is set; callers gate on `value.is_array()` so this is
/// never reached with a non-array value.
fn wrap_list_envelope(items: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": crate::API_VERSION,
        "kind": "List",
        "items": items,
    })
}

/// Route a `Doc` to `sink_stdout` per `format`. Returns `true` when the format
/// was handled (structured); returns `false` for `Table | Wide` so the caller
/// can fall back to the human renderer.
///
/// Data-dependent runtime failures (template render/context error, template-file
/// read error) are routed to `sink_stderr` — never to the `sink_stdout` data
/// channel — and set `output_error` so the process can exit non-zero. Syntax
/// errors (malformed jsonpath/template) are caught earlier at flag-parse time
/// (`OutputFormatArg::from_str`); the walker is panic-free regardless.
pub(crate) fn emit_structured(
    sink_stdout: &dyn Writer,
    sink_stderr: &dyn Writer,
    output_error: &std::sync::atomic::AtomicBool,
    doc: &Doc,
    format: &OutputFormat,
    list_envelope: bool,
) -> bool {
    match format {
        OutputFormat::Table | OutputFormat::Wide => false,
        OutputFormat::Json => {
            let mut v = doc.data_or_self_json();
            if list_envelope && v.is_array() {
                v = wrap_list_envelope(v);
            }
            let text = serde_json::to_string_pretty(&v).unwrap_or_default();
            sink_stdout.write_line(&text);
            true
        }
        OutputFormat::Yaml => {
            let mut v = doc.data_or_self_json();
            if list_envelope && v.is_array() {
                v = wrap_list_envelope(v);
            }
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
            render_template_to(sink_stdout, sink_stderr, output_error, doc, tmpl);
            true
        }
        OutputFormat::TemplateFile(path) => {
            let path = crate::expand_tilde(path);
            match std::fs::read_to_string(&path) {
                Ok(tmpl) => render_template_to(sink_stdout, sink_stderr, output_error, doc, &tmpl),
                Err(e) => {
                    sink_stderr.write_line(&format!(
                        "failed to read template file '{}': {}",
                        path.posix(),
                        clean_io_reason(&e)
                    ));
                    output_error.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
            true
        }
    }
}

/// Render a tera template against the doc's JSON to `sink_stdout`. Any render /
/// context failure (or a template-parse error, defense-in-depth — the parse is
/// validated at flag-parse time too) is written to `sink_stderr` and flips
/// `output_error`, so the data channel stays clean and the process exits
/// non-zero.
fn render_template_to(
    sink_stdout: &dyn Writer,
    sink_stderr: &dyn Writer,
    output_error: &std::sync::atomic::AtomicBool,
    doc: &Doc,
    template: &str,
) {
    let v = doc.data_or_self_json();
    let mut tera = tera::Tera::default();
    let tmpl_name = "__inline__";
    if let Err(e) = tera.add_raw_template(tmpl_name, template) {
        sink_stderr.write_line(&format!("invalid template: {e}"));
        output_error.store(true, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    let fail = |msg: String| {
        sink_stderr.write_line(&msg);
        output_error.store(true, std::sync::atomic::Ordering::Relaxed);
    };
    match v {
        serde_json::Value::Array(arr) => {
            for item in arr {
                match tera::Context::from_value(item.clone()) {
                    Ok(ctx) => match tera.render(tmpl_name, &ctx) {
                        Ok(rendered) => sink_stdout.write_line(&rendered),
                        Err(e) => fail(format!("template render error: {e}")),
                    },
                    Err(e) => fail(format!("template context error: {e}")),
                }
            }
        }
        other => match tera::Context::from_value(other) {
            Ok(ctx) => match tera.render(tmpl_name, &ctx) {
                Ok(rendered) => sink_stdout.write_line(&rendered),
                Err(e) => fail(format!("template render error: {e}")),
            },
            Err(e) => fail(format!("template context error: {e}")),
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

    /// Drive `emit_structured` with a discarded stderr sink and a fresh error
    /// flag, returning `(handled, output_error)`. Used by the happy-path tests
    /// that only care about the stdout buffer.
    fn emit(sink_stdout: &StringSink, doc: &Doc, fmt: &OutputFormat) -> (bool, bool) {
        emit_env(sink_stdout, doc, fmt, false)
    }

    /// Like `emit` but lets the caller set `list_envelope`, for the KRM List
    /// wrapper tests.
    fn emit_env(
        sink_stdout: &StringSink,
        doc: &Doc,
        fmt: &OutputFormat,
        list_envelope: bool,
    ) -> (bool, bool) {
        let stderr = StringSink(Arc::new(Mutex::new(String::new())));
        let flag = std::sync::atomic::AtomicBool::new(false);
        let handled = emit_structured(sink_stdout, &stderr, &flag, doc, fmt, list_envelope);
        (handled, flag.load(std::sync::atomic::Ordering::Relaxed))
    }

    /// Like `emit` but exposes the stderr buffer so error-routing tests can
    /// assert the message landed on stderr (not the stdout data channel).
    fn emit_with_stderr(
        sink_stdout: &StringSink,
        doc: &Doc,
        fmt: &OutputFormat,
    ) -> (bool, bool, String) {
        let stderr_buf = Arc::new(Mutex::new(String::new()));
        let stderr = StringSink(stderr_buf.clone());
        let flag = std::sync::atomic::AtomicBool::new(false);
        let handled = emit_structured(sink_stdout, &stderr, &flag, doc, fmt, false);
        (
            handled,
            flag.load(std::sync::atomic::Ordering::Relaxed),
            take(&stderr_buf),
        )
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

    // ---- validate_jsonpath_expr ---------------------------------------------

    #[test]
    fn validate_jsonpath_expr_accepts_wellformed() {
        for expr in [
            "{.a.b}",
            "{[0].name}",
            "{.items[*].x}",
            "{.items[0:2]}",
            ".nested.inner.field",
            "applyId",
            "{}",
            "",
            "{.items[0]}",
        ] {
            assert!(
                validate_jsonpath_expr(expr).is_ok(),
                "expected {expr:?} to validate"
            );
        }
    }

    #[test]
    fn validate_jsonpath_expr_rejects_malformed() {
        for expr in [
            "{.items[",   // missing closing bracket + brace
            ".items[",    // missing closing bracket
            "{.a[}",      // bracket closed by brace, never by ]
            "{.a]b}",     // stray closing bracket
            "{.items[]}", // empty bracket
            "a[",         // unbalanced
            "[",          // lone open
            "a[[0]]",     // nested brackets
        ] {
            assert!(
                validate_jsonpath_expr(expr).is_err(),
                "expected {expr:?} to be rejected"
            );
        }
    }

    #[test]
    fn validate_jsonpath_expr_message_names_bracket_not_wrapper() {
        // In `{.items[` the defect is the unterminated `[`, NOT the `{ }`
        // wrapper. The message must point at the real problem.
        let err = validate_jsonpath_expr("{.items[").unwrap_err();
        assert!(
            err.contains('['),
            "message should name the bracket, got: {err:?}"
        );
        assert!(
            !err.contains("wrapper"),
            "message must not misattribute to the wrapper, got: {err:?}"
        );

        // A genuine wrapper imbalance (brackets fine) still reports the wrapper.
        let err = validate_jsonpath_expr("{.a.b").unwrap_err();
        assert!(
            err.contains("wrapper"),
            "genuine wrapper imbalance should say 'wrapper', got: {err:?}"
        );
    }

    #[test]
    fn clean_io_reason_drops_os_error_tail() {
        use std::io::{Error, ErrorKind};
        let cases = [
            (ErrorKind::NotFound, "no such file"),
            (ErrorKind::PermissionDenied, "permission denied"),
            (ErrorKind::IsADirectory, "is a directory"),
        ];
        for (kind, expected) in cases {
            let reason = clean_io_reason(&Error::from(kind));
            assert_eq!(reason, expected, "for {kind:?}");
            assert!(
                !reason.contains("os error"),
                "must not leak '(os error N)', got: {reason:?}"
            );
        }
    }

    // ---- walk_jsonpath / apply_jsonpath panic-freedom ------------------------

    #[test]
    fn apply_jsonpath_never_panics_on_malformed_input() {
        // A table of structurally-nasty expressions, including a multibyte
        // segment, exercising the bracket-slice path that previously panicked
        // with "byte range starts at N but ends at M".
        let v = fixture();
        let arr = serde_json::json!([{"name": "base"}]);
        for expr in [
            "items[", "[", "]", "a[b]", "a[1:", "items]", ".items[", "{.items[", "é[", "[[", "]]",
            "items[]", "items[0", "items[*", ".[0]", "a[b][c]",
        ] {
            // Must not panic against either an object or a bare-array root.
            let _ = apply_jsonpath(&v, expr);
            let _ = apply_jsonpath(&arr, expr);
        }
    }

    // ---- bare-array model ---------------------------------------------------

    #[test]
    fn apply_jsonpath_bare_array_index_works() {
        // List commands emit a bare top-level array; the working expression is
        // `[0].name`, NOT the kubectl KRM `.items[0].name`.
        let arr = serde_json::json!([{"name": "base"}, {"name": "dev"}]);
        assert_eq!(apply_jsonpath(&arr, "{[0].name}"), "base");
    }

    #[test]
    fn apply_jsonpath_bare_array_items_envelope_returns_empty() {
        // The KRM `.items[...]` form silently returns empty against a bare
        // array — documents the real model so the doc fix is anchored to a test.
        let arr = serde_json::json!([{"name": "base"}, {"name": "dev"}]);
        assert_eq!(apply_jsonpath(&arr, "{.items[0].name}"), "");
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
        let (handled, _) = emit(&sink, &doc, &OutputFormat::Table);
        assert!(!handled);
        assert_eq!(take(&buf), "");
    }

    #[test]
    fn emit_structured_wide_returns_false_and_writes_nothing() {
        let (buf, sink) = capture();
        let doc = doc_with(serde_json::json!({"name": "x"}));
        let (handled, _) = emit(&sink, &doc, &OutputFormat::Wide);
        assert!(!handled);
        assert_eq!(take(&buf), "");
    }

    #[test]
    fn emit_structured_json_writes_pretty_json() {
        let (buf, sink) = capture();
        let payload = serde_json::json!({"name": "x", "n": 1});
        let doc = doc_with(payload.clone());
        let (handled, _) = emit(&sink, &doc, &OutputFormat::Json);
        assert!(handled);
        let expected = format!("{}\n", serde_json::to_string_pretty(&payload).unwrap());
        assert_eq!(take(&buf), expected);
    }

    #[test]
    fn emit_structured_yaml_strips_leading_doc_marker() {
        let (buf, sink) = capture();
        let payload = serde_json::json!({"name": "x", "n": 1});
        let doc = doc_with(payload.clone());
        let (handled, _) = emit(&sink, &doc, &OutputFormat::Yaml);
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
    fn emit_structured_json_array_with_list_envelope_wraps_in_krm_list() {
        let (buf, sink) = capture();
        let payload = serde_json::json!([
            {"name": "alpha", "value": 1},
            {"name": "beta", "value": 2},
        ]);
        let doc = doc_with(payload.clone());
        let (handled, _) = emit_env(&sink, &doc, &OutputFormat::Json, true);
        assert!(handled);
        let parsed: serde_json::Value = serde_json::from_str(&take(&buf)).unwrap();
        assert!(parsed.is_object(), "envelope must be a JSON object");
        assert_eq!(parsed["apiVersion"], "cfgd.io/v1alpha1");
        assert_eq!(parsed["kind"], "List");
        assert_eq!(parsed["items"], payload);
    }

    #[test]
    fn emit_structured_json_array_without_list_envelope_stays_bare() {
        let (buf, sink) = capture();
        let payload = serde_json::json!([
            {"name": "alpha"},
            {"name": "beta"},
        ]);
        let doc = doc_with(payload.clone());
        let (handled, _) = emit_env(&sink, &doc, &OutputFormat::Json, false);
        assert!(handled);
        let parsed: serde_json::Value = serde_json::from_str(&take(&buf)).unwrap();
        assert_eq!(parsed, payload, "default must be the bare array, unchanged");
    }

    #[test]
    fn emit_structured_json_object_with_list_envelope_is_unchanged() {
        let (buf, sink) = capture();
        let payload = serde_json::json!({"entries": [{"name": "alpha"}], "count": 1});
        let doc = doc_with(payload.clone());
        let (handled, _) = emit_env(&sink, &doc, &OutputFormat::Json, true);
        assert!(handled);
        let parsed: serde_json::Value = serde_json::from_str(&take(&buf)).unwrap();
        assert_eq!(parsed, payload, "objects are never wrapped");
    }

    #[test]
    fn emit_structured_yaml_array_with_list_envelope_wraps_in_krm_list() {
        let (buf, sink) = capture();
        let payload = serde_json::json!([
            {"name": "alpha"},
            {"name": "beta"},
        ]);
        let doc = doc_with(payload.clone());
        let (handled, _) = emit_env(&sink, &doc, &OutputFormat::Yaml, true);
        assert!(handled);
        let parsed: serde_json::Value = serde_yaml::from_str(&take(&buf)).unwrap();
        assert_eq!(parsed["apiVersion"], "cfgd.io/v1alpha1");
        assert_eq!(parsed["kind"], "List");
        assert_eq!(parsed["items"], payload);
    }

    #[test]
    fn emit_structured_yaml_array_without_list_envelope_stays_bare() {
        let (buf, sink) = capture();
        let payload = serde_json::json!([
            {"name": "alpha"},
            {"name": "beta"},
        ]);
        let doc = doc_with(payload.clone());
        let (handled, _) = emit_env(&sink, &doc, &OutputFormat::Yaml, false);
        assert!(handled);
        let parsed: serde_json::Value = serde_yaml::from_str(&take(&buf)).unwrap();
        assert_eq!(parsed, payload, "default must be the bare array, unchanged");
    }

    #[test]
    fn emit_structured_name_array_ignores_list_envelope() {
        let payload = serde_json::json!([
            {"name": "alpha"},
            {"name": "beta"},
        ]);
        let (buf_off, sink_off) = capture();
        let (buf_on, sink_on) = capture();
        let doc = doc_with(payload);
        emit_env(&sink_off, &doc, &OutputFormat::Name, false);
        emit_env(&sink_on, &doc, &OutputFormat::Name, true);
        assert_eq!(
            take(&buf_on),
            take(&buf_off),
            "list_envelope must not affect non-json/yaml formats"
        );
        assert_eq!(take(&buf_on), "alpha\nbeta\n");
    }

    #[test]
    fn emit_structured_name_writes_name_for_object() {
        let (buf, sink) = capture();
        let doc = doc_with(serde_json::json!({"name": "x"}));
        let (handled, _) = emit(&sink, &doc, &OutputFormat::Name);
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
        let (handled, _) = emit(&sink, &doc, &OutputFormat::Name);
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
        let (handled, _) = emit(&sink, &doc, &OutputFormat::Name);
        assert!(handled);
        assert_eq!(take(&buf), "alpha\ngamma\n");
    }

    #[test]
    fn emit_structured_jsonpath_writes_result_of_apply_jsonpath() {
        let (buf, sink) = capture();
        let doc = doc_with(serde_json::json!({"name": "x"}));
        let (handled, _) = emit(&sink, &doc, &OutputFormat::Jsonpath("{.name}".into()));
        assert!(handled);
        assert_eq!(take(&buf), "x\n");
    }

    #[test]
    fn emit_structured_template_renders_inline_template() {
        let (buf, sink) = capture();
        let doc = doc_with(serde_json::json!({"name": "alpha"}));
        let (handled, _) = emit(
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
        let (handled, _) = emit(&sink, &doc, &OutputFormat::Template("{{ name }}".into()));
        assert!(handled);
        assert_eq!(take(&buf), "alpha\nbeta\n");
    }

    #[test]
    fn emit_structured_template_invalid_template_routes_to_stderr_and_flags() {
        let (stdout_buf, sink) = capture();
        let doc = doc_with(serde_json::json!({"name": "x"}));
        let (handled, errored, stderr) =
            emit_with_stderr(&sink, &doc, &OutputFormat::Template("{{ ".into()));
        assert!(handled);
        assert!(errored, "parse failure must set the output-error flag");
        assert_eq!(take(&stdout_buf), "", "data channel must stay clean");
        assert!(
            stderr.starts_with("invalid template: "),
            "expected 'invalid template:' on stderr, got: {stderr:?}"
        );
    }

    #[test]
    fn emit_structured_template_render_error_routes_to_stderr_and_flags() {
        let (stdout_buf, sink) = capture();
        let doc = doc_with(serde_json::json!({"foo": "bar"}));
        let (handled, errored, stderr) = emit_with_stderr(
            &sink,
            &doc,
            &OutputFormat::Template("{{ foo | NONEXISTENT }}".into()),
        );
        assert!(handled);
        assert!(errored, "render failure must set the output-error flag");
        assert_eq!(take(&stdout_buf), "", "data channel must stay clean");
        assert!(
            stderr.starts_with("template render error: "),
            "expected 'template render error:' on stderr, got: {stderr:?}"
        );
    }

    #[test]
    fn emit_structured_template_file_missing_routes_to_stderr_and_flags() {
        let (stdout_buf, sink) = capture();
        let doc = doc_with(serde_json::json!({"name": "x"}));
        let missing = std::path::PathBuf::from("/nonexistent/cfgd-structured-test/path.tmpl");
        let (handled, errored, stderr) =
            emit_with_stderr(&sink, &doc, &OutputFormat::TemplateFile(missing));
        assert!(handled);
        assert!(errored, "file-read failure must set the output-error flag");
        assert_eq!(take(&stdout_buf), "", "data channel must stay clean");
        assert!(
            stderr.starts_with("failed to read template file"),
            "expected 'failed to read template file' on stderr, got: {stderr:?}"
        );
        assert!(
            stderr.contains("no such file"),
            "expected the cleaned NotFound reason, got: {stderr:?}"
        );
        assert!(
            !stderr.contains("os error"),
            "must not leak '(os error N)', got: {stderr:?}"
        );
    }

    #[test]
    fn emit_structured_template_file_reads_and_renders() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("greeting.tera");
        std::fs::write(&path, "hello {{ name }}").unwrap();
        let (buf, sink) = capture();
        let doc = doc_with(serde_json::json!({"name": "alpha"}));
        let (handled, _) = emit(&sink, &doc, &OutputFormat::TemplateFile(path));
        assert!(handled);
        assert_eq!(take(&buf), "hello alpha\n");
    }
}
