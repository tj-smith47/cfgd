use serde::Serialize;

use super::{OutputFormat, Printer};

/// Apply a kubectl-compatible jsonpath expression to a JSON value.
///
/// Supports:
/// - `{.field.nested}` — dot-path into objects
/// - `{.items[0]}` — array index
/// - `{.items[*].name}` — wildcard collect
/// - `{.items[0:3]}` — array slice
///
/// Scalars return as raw text, objects/arrays as JSON.
pub(super) fn apply_jsonpath(value: &serde_json::Value, expr: &str) -> String {
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
pub(super) fn name_from_value(value: &serde_json::Value) -> Option<String> {
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
pub(super) fn split_jsonpath_segment(path: &str) -> (&str, &str) {
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
pub(super) fn format_jsonpath_result(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => serde_json::to_string_pretty(value).unwrap_or_default(),
    }
}

impl Printer {
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
}
