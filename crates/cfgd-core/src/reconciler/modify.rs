//! Format-aware partial-file modification for `strategy: Modify`.
//!
//! Where `Copy`/`Symlink`/`Template` own the *whole* target file, `Modify`
//! owns only the keys it names: it reads the target's current content, folds
//! `modify.ensure` into it (or pipes it through `modify.script`), and returns
//! the new content. Everything the spec does not mention survives untouched.
//!
//! Format preservation is per-format and deliberate:
//!
//! | Format | Engine | Comments preserved |
//! |---|---|---|
//! | INI | line-preserving editor in this module | yes |
//! | TOML | `toml_edit` | yes |
//! | JSON | `serde_json` (reflowed) | n/a — JSON has none |
//! | YAML | `serde_yaml` (reflowed) | **no** — use `script` when comments matter |

use std::path::{Path, PathBuf};

use crate::config::{ModifyFormat, ModifySpec};
use crate::errors::{FileError, Result};

use super::scripts::{MODULE_SCRIPT_TIMEOUT, run_filter_script, script_default_workdir};

/// Execution context for `modify.script`, ignored by `modify.ensure`.
///
/// `script_dir` anchors a relative script path (the module's directory, or the
/// config directory for a profile-level file). The working directory and
/// environment default to the same values a lifecycle script would receive.
pub struct ModifyContext<'a> {
    script_dir: &'a Path,
    working_dir: Option<&'a Path>,
    env: &'a [(String, String)],
    timeout: std::time::Duration,
}

impl<'a> ModifyContext<'a> {
    /// Context anchored at `script_dir`, with the default script working
    /// directory (the user's home), no extra environment, and the standard
    /// module-script timeout.
    pub fn new(script_dir: &'a Path) -> Self {
        Self {
            script_dir,
            working_dir: None,
            env: &[],
            timeout: MODULE_SCRIPT_TIMEOUT,
        }
    }

    /// Run the script in `dir` instead of the user's home directory.
    pub fn with_working_dir(mut self, dir: &'a Path) -> Self {
        self.working_dir = Some(dir);
        self
    }

    /// Inject `env` into the script's process environment (the `CFGD_*`
    /// metadata a lifecycle script receives, plus the module's `spec.env`).
    pub fn with_env(mut self, env: &'a [(String, String)]) -> Self {
        self.env = env;
        self
    }

    /// Bound the script's runtime.
    pub fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

/// Compute the new content of `target` by applying `spec` to `current`.
///
/// A missing target is passed in as an empty `current`: `ensure` then creates a
/// minimal document and `script` receives empty stdin. The function is pure
/// with respect to the filesystem for `ensure` mode — it never reads or writes
/// the target — so the same call produces both the plan preview and the applied
/// content.
pub fn compute_modified(
    current: &str,
    spec: &ModifySpec,
    target: &Path,
    ctx: &ModifyContext<'_>,
) -> Result<String> {
    match (spec.ensure.as_ref(), spec.script.as_deref()) {
        (Some(ensure), None) => match resolve_format(spec, target)? {
            ModifyFormat::Ini => merge_ini(current, ensure, target),
            ModifyFormat::Json => merge_json(current, ensure, target),
            ModifyFormat::Yaml => merge_yaml(current, ensure, target),
            ModifyFormat::Toml => merge_toml(current, ensure, target),
        },
        (None, Some(script)) => run_script(current, script, target, ctx),
        _ => Err(FileError::ModifySpecInvalid {
            path: target.to_path_buf(),
        }
        .into()),
    }
}

/// Resolve the format to parse the target as: the explicit `modify.format`
/// when set, otherwise inferred from the target's extension.
pub fn resolve_format(spec: &ModifySpec, target: &Path) -> Result<ModifyFormat> {
    match spec.format {
        Some(format) => Ok(format),
        None => infer_format(target).ok_or_else(|| {
            FileError::ModifyFormatUnknown {
                path: target.to_path_buf(),
            }
            .into()
        }),
    }
}

/// Infer a [`ModifyFormat`] from a target's extension. `None` for any other
/// extension (and for extensionless paths) — the caller turns that into a
/// typed error telling the author to set `modify.format`.
pub fn infer_format(target: &Path) -> Option<ModifyFormat> {
    let ext = target.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "ini" => Some(ModifyFormat::Ini),
        "json" => Some(ModifyFormat::Json),
        "yaml" | "yml" => Some(ModifyFormat::Yaml),
        "toml" => Some(ModifyFormat::Toml),
        _ => None,
    }
}

fn format_label(format: ModifyFormat) -> &'static str {
    match format {
        ModifyFormat::Ini => "ini",
        ModifyFormat::Json => "json",
        ModifyFormat::Yaml => "yaml",
        ModifyFormat::Toml => "toml",
    }
}

fn parse_error(target: &Path, format: ModifyFormat, message: impl std::fmt::Display) -> FileError {
    FileError::ModifyParse {
        path: target.to_path_buf(),
        format: format_label(format).to_string(),
        message: message.to_string(),
    }
}

fn serialize_error(
    target: &Path,
    format: ModifyFormat,
    message: impl std::fmt::Display,
) -> FileError {
    FileError::ModifySerialize {
        path: target.to_path_buf(),
        format: format_label(format).to_string(),
        message: message.to_string(),
    }
}

fn shape_error(target: &Path, format: ModifyFormat, message: impl Into<String>) -> FileError {
    FileError::ModifyEnsureShape {
        path: target.to_path_buf(),
        format: format_label(format).to_string(),
        message: message.into(),
    }
}

/// `ensure` must be a mapping for every format: a scalar or list at the top
/// level would replace the entire document, which is the opposite of what the
/// `Modify` strategy promises.
fn ensure_mapping<'v>(
    ensure: &'v serde_yaml::Value,
    target: &Path,
    format: ModifyFormat,
) -> Result<&'v serde_yaml::Mapping> {
    ensure
        .as_mapping()
        .ok_or_else(|| shape_error(target, format, "'ensure' must be a mapping of keys").into())
}

// ---------------------------------------------------------------------------
// YAML
// ---------------------------------------------------------------------------

fn merge_yaml(current: &str, ensure: &serde_yaml::Value, target: &Path) -> Result<String> {
    let overlay = ensure_mapping(ensure, target, ModifyFormat::Yaml)?;
    let mut doc: serde_yaml::Value = if current.trim().is_empty() {
        serde_yaml::Value::Mapping(serde_yaml::Mapping::new())
    } else {
        serde_yaml::from_str::<serde_yaml::Value>(current)
            .map_err(|e| parse_error(target, ModifyFormat::Yaml, e))?
    };
    // An all-comment document parses to Null; merging into it would drop the
    // ensured keys on the floor.
    if doc.is_null() {
        doc = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    }
    if !doc.is_mapping() {
        return Err(parse_error(
            target,
            ModifyFormat::Yaml,
            "top-level document is not a mapping",
        )
        .into());
    }
    crate::deep_merge_yaml(&mut doc, &serde_yaml::Value::Mapping(overlay.clone()));
    serde_yaml::to_string(&doc).map_err(|e| serialize_error(target, ModifyFormat::Yaml, e).into())
}

// ---------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------

fn merge_json(current: &str, ensure: &serde_yaml::Value, target: &Path) -> Result<String> {
    let overlay_map = ensure_mapping(ensure, target, ModifyFormat::Json)?;
    let overlay = yaml_to_json(
        &serde_yaml::Value::Mapping(overlay_map.clone()),
        target,
        ModifyFormat::Json,
    )?;
    let mut doc: serde_json::Value = if current.trim().is_empty() {
        serde_json::Value::Object(serde_json::Map::new())
    } else {
        serde_json::from_str(current).map_err(|e| parse_error(target, ModifyFormat::Json, e))?
    };
    if !doc.is_object() {
        return Err(parse_error(
            target,
            ModifyFormat::Json,
            "top-level document is not an object",
        )
        .into());
    }
    deep_merge_json(&mut doc, &overlay);
    let mut out = serde_json::to_string_pretty(&doc)
        .map_err(|e| serialize_error(target, ModifyFormat::Json, e))?;
    out.push('\n');
    Ok(out)
}

/// Deep merge two JSON values. Objects merge recursively; every other type is
/// replaced by the overlay. Mirrors [`crate::deep_merge_yaml`] so the two
/// structured formats behave identically.
fn deep_merge_json(base: &mut serde_json::Value, overlay: &serde_json::Value) {
    match (base, overlay) {
        (serde_json::Value::Object(base_map), serde_json::Value::Object(overlay_map)) => {
            for (key, value) in overlay_map {
                match base_map.get_mut(key) {
                    Some(base_value) => deep_merge_json(base_value, value),
                    None => {
                        base_map.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (base, overlay) => *base = overlay.clone(),
    }
}

fn yaml_to_json(
    value: &serde_yaml::Value,
    target: &Path,
    format: ModifyFormat,
) -> Result<serde_json::Value> {
    Ok(match value {
        serde_yaml::Value::Null => serde_json::Value::Null,
        serde_yaml::Value::Bool(b) => serde_json::Value::Bool(*b),
        serde_yaml::Value::Number(n) => number_to_json(n)
            .ok_or_else(|| shape_error(target, format, format!("unrepresentable number: {n:?}")))?,
        serde_yaml::Value::String(s) => serde_json::Value::String(s.clone()),
        serde_yaml::Value::Sequence(seq) => serde_json::Value::Array(
            seq.iter()
                .map(|v| yaml_to_json(v, target, format))
                .collect::<Result<Vec<_>>>()?,
        ),
        serde_yaml::Value::Mapping(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                let key = k
                    .as_str()
                    .ok_or_else(|| shape_error(target, format, "object keys must be strings"))?;
                out.insert(key.to_string(), yaml_to_json(v, target, format)?);
            }
            serde_json::Value::Object(out)
        }
        serde_yaml::Value::Tagged(tagged) => yaml_to_json(&tagged.value, target, format)?,
    })
}

fn number_to_json(n: &serde_yaml::Number) -> Option<serde_json::Value> {
    if let Some(i) = n.as_i64() {
        return Some(serde_json::Value::Number(i.into()));
    }
    if let Some(u) = n.as_u64() {
        return Some(serde_json::Value::Number(u.into()));
    }
    n.as_f64()
        .and_then(serde_json::Number::from_f64)
        .map(serde_json::Value::Number)
}

// ---------------------------------------------------------------------------
// TOML
// ---------------------------------------------------------------------------

fn merge_toml(current: &str, ensure: &serde_yaml::Value, target: &Path) -> Result<String> {
    let overlay = ensure_mapping(ensure, target, ModifyFormat::Toml)?;
    let mut doc: toml_edit::DocumentMut = if current.trim().is_empty() {
        toml_edit::DocumentMut::new()
    } else {
        current
            .parse()
            .map_err(|e| parse_error(target, ModifyFormat::Toml, e))?
    };
    merge_toml_table(doc.as_table_mut(), overlay, target)?;
    let mut out = doc.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

/// What a table slot currently holds, decided before taking a mutable borrow.
enum TomlSlot {
    Vacant,
    Table,
    InlineTable,
    Other,
}

fn toml_slot(item: Option<&toml_edit::Item>) -> TomlSlot {
    match item {
        None | Some(toml_edit::Item::None) => TomlSlot::Vacant,
        Some(item) if item.is_table() => TomlSlot::Table,
        Some(item) if item.is_inline_table() => TomlSlot::InlineTable,
        Some(_) => TomlSlot::Other,
    }
}

fn missing_toml_slot(target: &Path, key: &str) -> FileError {
    shape_error(
        target,
        ModifyFormat::Toml,
        format!("could not open table '{key}'"),
    )
}

fn merge_toml_table(
    table: &mut toml_edit::Table,
    overlay: &serde_yaml::Mapping,
    target: &Path,
) -> Result<()> {
    for (key, value) in overlay {
        let key = toml_key(key, target)?;
        match value {
            serde_yaml::Value::Mapping(sub) => match toml_slot(table.get(key)) {
                TomlSlot::Table => {
                    let Some(existing) = table.get_mut(key).and_then(toml_edit::Item::as_table_mut)
                    else {
                        return Err(missing_toml_slot(target, key).into());
                    };
                    merge_toml_table(existing, sub, target)?;
                }
                TomlSlot::InlineTable => {
                    let Some(existing) = table
                        .get_mut(key)
                        .and_then(toml_edit::Item::as_inline_table_mut)
                    else {
                        return Err(missing_toml_slot(target, key).into());
                    };
                    merge_toml_inline_table(existing, sub, target)?;
                }
                // Either the key is absent, or the target holds a scalar/array
                // where the spec wants a table — the ensured shape wins.
                // Removing first drops the old entry's decor: reusing the slot
                // would carry a scalar key's spacing onto the new `[table]`
                // header (`build = 4` → `[build ]`).
                TomlSlot::Vacant | TomlSlot::Other => {
                    let mut fresh = toml_edit::Table::new();
                    // A table cfgd creates is written out explicitly; an
                    // implicit one would vanish when it holds only sub-tables.
                    fresh.set_implicit(false);
                    merge_toml_table(&mut fresh, sub, target)?;
                    table.remove(key);
                    table.insert(key, toml_edit::Item::Table(fresh));
                }
            },
            _ => {
                let new_value = yaml_to_toml(value, target)?;
                set_toml_value(table.entry(key), new_value);
            }
        }
    }
    Ok(())
}

fn merge_toml_inline_table(
    table: &mut toml_edit::InlineTable,
    overlay: &serde_yaml::Mapping,
    target: &Path,
) -> Result<()> {
    for (key, value) in overlay {
        let key = toml_key(key, target)?;
        match value {
            serde_yaml::Value::Mapping(sub) => {
                if let Some(nested) = table.get_mut(key).and_then(|v| v.as_inline_table_mut()) {
                    merge_toml_inline_table(nested, sub, target)?;
                } else {
                    let mut fresh = toml_edit::InlineTable::new();
                    merge_toml_inline_table(&mut fresh, sub, target)?;
                    table.insert(key, toml_edit::Value::InlineTable(fresh));
                }
            }
            _ => {
                let new_value = yaml_to_toml(value, target)?;
                match table.get_mut(key) {
                    Some(old) => {
                        let decor = old.decor().clone();
                        let mut replacement = new_value;
                        *replacement.decor_mut() = decor;
                        *old = replacement;
                    }
                    None => {
                        table.insert(key, new_value);
                    }
                }
            }
        }
    }
    Ok(())
}

/// Assign a value into a table entry, carrying over the existing value's decor
/// so an updated key keeps its surrounding whitespace and trailing comment.
fn set_toml_value(entry: toml_edit::Entry<'_>, new_value: toml_edit::Value) {
    match entry {
        toml_edit::Entry::Occupied(mut occupied) => {
            let item = occupied.get_mut();
            let decor = item.as_value().map(|old| old.decor().clone());
            let mut replacement = new_value;
            if let Some(decor) = decor {
                *replacement.decor_mut() = decor;
            }
            *item = toml_edit::Item::Value(replacement);
        }
        toml_edit::Entry::Vacant(vacant) => {
            vacant.insert(toml_edit::Item::Value(new_value));
        }
    }
}

fn toml_key<'k>(key: &'k serde_yaml::Value, target: &Path) -> Result<&'k str> {
    key.as_str()
        .ok_or_else(|| shape_error(target, ModifyFormat::Toml, "table keys must be strings").into())
}

fn yaml_to_toml(value: &serde_yaml::Value, target: &Path) -> Result<toml_edit::Value> {
    Ok(match value {
        serde_yaml::Value::Bool(b) => toml_edit::Value::from(*b),
        serde_yaml::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                toml_edit::Value::from(i)
            } else if let Some(f) = n.as_f64() {
                toml_edit::Value::from(f)
            } else {
                return Err(shape_error(
                    target,
                    ModifyFormat::Toml,
                    format!("unrepresentable number: {n:?}"),
                )
                .into());
            }
        }
        serde_yaml::Value::String(s) => toml_edit::Value::from(s.as_str()),
        serde_yaml::Value::Sequence(seq) => {
            let mut array = toml_edit::Array::new();
            for item in seq {
                array.push(yaml_to_toml(item, target)?);
            }
            toml_edit::Value::Array(array)
        }
        serde_yaml::Value::Mapping(map) => {
            let mut inline = toml_edit::InlineTable::new();
            merge_toml_inline_table(&mut inline, map, target)?;
            toml_edit::Value::InlineTable(inline)
        }
        serde_yaml::Value::Tagged(tagged) => yaml_to_toml(&tagged.value, target)?,
        serde_yaml::Value::Null => {
            return Err(shape_error(
                target,
                ModifyFormat::Toml,
                "TOML has no null — remove the key or give it a value",
            )
            .into());
        }
    })
}

// ---------------------------------------------------------------------------
// INI
// ---------------------------------------------------------------------------

/// Section → key → value merge over a line-oriented INI document.
///
/// A scalar at the top level of `ensure` targets a key in the file's global
/// area (before the first `[section]` header); a mapping targets a section.
fn merge_ini(current: &str, ensure: &serde_yaml::Value, target: &Path) -> Result<String> {
    let overlay = ensure_mapping(ensure, target, ModifyFormat::Ini)?;
    let mut doc = IniDoc::parse(current);
    for (key, value) in overlay {
        let name = key.as_str().ok_or_else(|| {
            shape_error(
                target,
                ModifyFormat::Ini,
                "section and key names must be strings",
            )
        })?;
        match value {
            serde_yaml::Value::Mapping(section) => {
                let mut pairs = Vec::with_capacity(section.len());
                for (k, v) in section {
                    let k = k.as_str().ok_or_else(|| {
                        shape_error(target, ModifyFormat::Ini, "key names must be strings")
                    })?;
                    pairs.push((k, ini_scalar(v, target, &format!("{name}.{k}"))?));
                }
                doc.set_section_keys(name, &pairs);
            }
            other => {
                let rendered = ini_scalar(other, target, name)?;
                doc.set_global_key(name, &rendered);
            }
        }
    }
    Ok(doc.render())
}

/// Render an `ensure` value as an INI value. INI has no native list or nested
/// mapping syntax, so those are rejected rather than guessed at.
fn ini_scalar(value: &serde_yaml::Value, target: &Path, key: &str) -> Result<String> {
    match value {
        serde_yaml::Value::Bool(b) => Ok(b.to_string()),
        serde_yaml::Value::Number(n) => Ok(n.to_string()),
        serde_yaml::Value::String(s) => Ok(s.clone()),
        serde_yaml::Value::Tagged(tagged) => ini_scalar(&tagged.value, target, key),
        serde_yaml::Value::Null => Err(shape_error(
            target,
            ModifyFormat::Ini,
            format!("'{key}' has no value — INI cannot express null"),
        )
        .into()),
        serde_yaml::Value::Sequence(_) => Err(shape_error(
            target,
            ModifyFormat::Ini,
            format!("'{key}' is a list — INI supports section → key → scalar only"),
        )
        .into()),
        serde_yaml::Value::Mapping(_) => Err(shape_error(
            target,
            ModifyFormat::Ini,
            format!("'{key}' is nested — INI supports section → key → scalar only"),
        )
        .into()),
    }
}

/// A parsed-but-not-reformatted INI document: the original lines verbatim, plus
/// the line-ending facts needed to reproduce them.
struct IniDoc {
    lines: Vec<String>,
    crlf: bool,
    trailing_newline: bool,
}

impl IniDoc {
    fn parse(current: &str) -> Self {
        let crlf = current.contains("\r\n");
        let (body, trailing_newline) = match current.strip_suffix('\n') {
            Some(body) => (body, true),
            None => (current, false),
        };
        let lines = if body.is_empty() && !trailing_newline {
            Vec::new()
        } else {
            body.split('\n').map(str::to_string).collect()
        };
        Self {
            lines,
            crlf,
            // An empty target becomes a POSIX text file, not a file whose last
            // line lacks a newline.
            trailing_newline: trailing_newline || current.is_empty(),
        }
    }

    fn render(&self) -> String {
        let mut out = self.lines.join("\n");
        if self.trailing_newline {
            out.push('\n');
        }
        out
    }

    fn newline_suffix(&self) -> &'static str {
        if self.crlf { "\r" } else { "" }
    }

    /// Half-open line range of the global area: everything before the first
    /// section header.
    fn global_range(&self) -> (usize, usize) {
        let end = self
            .lines
            .iter()
            .position(|l| ini_section_name(l).is_some())
            .unwrap_or(self.lines.len());
        (0, end)
    }

    /// Half-open line range of `name`'s body (excluding its header line).
    fn section_range(&self, name: &str) -> Option<(usize, usize)> {
        let header = self
            .lines
            .iter()
            .position(|l| ini_section_name(l) == Some(name))?;
        let end = self.lines[header + 1..]
            .iter()
            .position(|l| ini_section_name(l).is_some())
            .map(|offset| header + 1 + offset)
            .unwrap_or(self.lines.len());
        Some((header + 1, end))
    }

    fn set_global_key(&mut self, key: &str, value: &str) {
        let (start, end) = self.global_range();
        if self.update_in_range(start, end, key, value) {
            return;
        }
        let sep = self.separator_style(start, end);
        let line = format!("{key}{sep}{value}{}", self.newline_suffix());
        let at = match self.last_key_line(start, end) {
            Some(idx) => idx + 1,
            // No global keys yet: land below a leading comment banner rather
            // than above it, but still above the first section.
            None => {
                let mut idx = start;
                while idx < end && is_comment_or_blank(&self.lines[idx]) {
                    idx += 1;
                }
                idx
            }
        };
        self.lines.insert(at, line);
        self.trailing_newline = true;
    }

    fn set_section_keys(&mut self, section: &str, pairs: &[(&str, String)]) {
        let Some((start, end)) = self.section_range(section) else {
            self.append_section(section, pairs);
            return;
        };
        let mut end = end;
        for (key, value) in pairs {
            if self.update_in_range(start, end, key, value) {
                continue;
            }
            let sep = self.separator_style(start, end);
            let line = format!("{key}{sep}{value}{}", self.newline_suffix());
            let at = match self.last_key_line(start, end) {
                Some(idx) => idx + 1,
                None => start,
            };
            self.lines.insert(at, line);
            end += 1;
            self.trailing_newline = true;
        }
    }

    fn append_section(&mut self, section: &str, pairs: &[(&str, String)]) {
        let nl = self.newline_suffix();
        if self.lines.iter().any(|l| !l.trim().is_empty()) {
            self.lines.push(nl.to_string());
        }
        self.lines.push(format!("[{section}]{nl}"));
        let sep = self.separator_style(0, self.lines.len());
        for (key, value) in pairs {
            self.lines.push(format!("{key}{sep}{value}{nl}"));
        }
        self.trailing_newline = true;
    }

    /// Rewrite every occurrence of `key` in the range; returns whether any
    /// existed. All duplicates are rewritten so the ensured value wins no
    /// matter which duplicate the consuming parser honours.
    fn update_in_range(&mut self, start: usize, end: usize, key: &str, value: &str) -> bool {
        let mut found = false;
        for idx in start..end.min(self.lines.len()) {
            if ini_key_name(&self.lines[idx]) == Some(key) {
                self.lines[idx] = ini_replace_value(&self.lines[idx], value);
                found = true;
            }
        }
        found
    }

    /// Index of the last `key = value` line in the range. Trailing blank lines
    /// and the comment block that introduces the *next* section are skipped, so
    /// an inserted key lands with its own section's keys.
    fn last_key_line(&self, start: usize, end: usize) -> Option<usize> {
        (start..end.min(self.lines.len()))
            .rev()
            .find(|idx| ini_key_name(&self.lines[*idx]).is_some())
    }

    /// `key = value` or `key=value`, whichever the neighbouring keys use.
    fn separator_style(&self, start: usize, end: usize) -> &'static str {
        let sample = (start..end.min(self.lines.len()))
            .find(|idx| ini_key_name(&self.lines[*idx]).is_some())
            .or_else(|| {
                (0..self.lines.len()).find(|idx| ini_key_name(&self.lines[*idx]).is_some())
            });
        match sample {
            Some(idx) => {
                let line = self.lines[idx].trim_end_matches('\r');
                match line.find('=') {
                    Some(eq) if line[..eq].ends_with(' ') => " = ",
                    Some(_) => "=",
                    None => " = ",
                }
            }
            None => " = ",
        }
    }
}

fn is_comment_or_blank(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';')
}

/// The section name of a `[name]` header line, or `None` for any other line.
fn ini_section_name(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    let inner = trimmed.strip_prefix('[')?.strip_suffix(']')?;
    Some(inner.trim())
}

/// The key of a `key = value` line, or `None` for comments, blanks, headers,
/// and lines without a `=`.
fn ini_key_name(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    if is_comment_or_blank(trimmed) || trimmed.starts_with('[') {
        return None;
    }
    let (key, _) = trimmed.split_once('=')?;
    let key = key.trim();
    if key.is_empty() { None } else { Some(key) }
}

/// Replace the value of a `key = value` line, preserving the key text, the
/// spacing around `=`, and the line ending. Anything after `=` (including an
/// inline comment) is replaced — INI dialects disagree on whether trailing
/// `#`/`;` starts a comment, so cfgd never tries to keep part of a value.
fn ini_replace_value(line: &str, value: &str) -> String {
    let (body, cr) = match line.strip_suffix('\r') {
        Some(body) => (body, "\r"),
        None => (line, ""),
    };
    let Some(eq) = body.find('=') else {
        return line.to_string();
    };
    let spacing: String = body[eq + 1..]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();
    format!("{}={}{}{}", &body[..eq], spacing, value, cr)
}

// ---------------------------------------------------------------------------
// Script mode
// ---------------------------------------------------------------------------

fn run_script(
    current: &str,
    script: &str,
    target: &Path,
    ctx: &ModifyContext<'_>,
) -> Result<String> {
    let default_workdir: PathBuf;
    let working_dir = match ctx.working_dir {
        Some(dir) => dir,
        None => {
            default_workdir = script_default_workdir(ctx.script_dir);
            &default_workdir
        }
    };

    let outcome = run_filter_script(
        script,
        ctx.script_dir,
        working_dir,
        ctx.env,
        current,
        ctx.timeout,
    )?;

    if outcome.timed_out {
        return Err(FileError::ModifyScriptFailed {
            path: target.to_path_buf(),
            script: script.to_string(),
            message: format!("timed out after {}s", ctx.timeout.as_secs()),
        }
        .into());
    }
    if !outcome.success {
        let exit = match outcome.exit_code {
            Some(code) => format!("exit {code}"),
            None => "killed by signal".to_string(),
        };
        let message = if outcome.stderr.is_empty() {
            exit
        } else {
            format!("{exit}: {}", outcome.stderr)
        };
        return Err(FileError::ModifyScriptFailed {
            path: target.to_path_buf(),
            script: script.to_string(),
            message,
        }
        .into());
    }
    Ok(outcome.stdout)
}

#[cfg(test)]
mod tests;
