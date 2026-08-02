//! Format-aware partial-file patching for `strategy: Patch`.
//!
//! Where `Copy`/`Symlink`/`Template` own the *whole* target file, `Patch`
//! owns only the keys it names: it reads the target's current content, folds
//! `patch.ensure` into it (or pipes it through `patch.script`), and returns
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

use crate::config::{PatchFormat, PatchSpec};
use crate::effective::Origin;
use crate::errors::{FileError, Result};
use crate::modules::ResolvedModule;

use super::scripts::{
    MODULE_SCRIPT_TIMEOUT, build_module_script_env, run_filter_script, script_default_workdir,
};
use super::types::{ReconcileContext, ScriptPhase};

/// Execution context for `patch.script`, ignored by `patch.ensure`.
///
/// `script_dir` anchors a relative script path (the module's directory, or the
/// config directory for a profile-level file). The working directory and
/// environment default to the same values a lifecycle script would receive.
pub struct PatchContext<'a> {
    script_dir: &'a Path,
    working_dir: Option<&'a Path>,
    env: &'a [(String, String)],
    timeout: std::time::Duration,
}

impl<'a> PatchContext<'a> {
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

    /// Inject `env` into the script's process environment.
    ///
    /// Required for a filter to see the `CFGD_*` metadata a lifecycle script
    /// gets: cfgd-core cannot synthesize it here (it needs the config dir,
    /// profile name, and phase), so a dispatch site must pass the output of
    /// `build_module_script_env` through. Without it the script runs with only
    /// the inherited process environment.
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

/// Owner of the values a [`PatchContext`] borrows, so every dispatch site
/// builds the same context from the same two inputs instead of re-deriving a
/// script directory and environment by hand.
///
/// Getting the script directory wrong is silent: a relative `script:` path that
/// does not resolve under it falls back to inline-command execution, so the
/// operator sees "command not found" instead of their script running. The two
/// constructors encode the only two correct answers — a module-deployed file
/// resolves against the module's directory, a profile-declared file against the
/// config directory.
#[derive(Debug)]
pub struct PatchBinding {
    script_dir: PathBuf,
    env: Vec<(String, String)>,
}

impl PatchBinding {
    /// Binding for a file declared by the profile (`spec.files.managed`):
    /// scripts resolve against the config directory and see the standard
    /// `CFGD_*` metadata with no module attribution.
    pub fn profile(config_dir: &Path, profile_name: &str, context: ReconcileContext) -> Self {
        Self {
            script_dir: config_dir.to_path_buf(),
            env: build_module_script_env(
                config_dir,
                profile_name,
                context,
                &ScriptPhase::Patch,
                None,
                None,
                &[],
            ),
        }
    }

    /// Binding for a file deployed by a module (`spec.files` in `module.yaml`):
    /// scripts resolve against the module's directory and additionally see
    /// `CFGD_MODULE_NAME`, `CFGD_MODULE_DIR`, and the module's declared `env`.
    pub fn module(
        config_dir: &Path,
        profile_name: &str,
        context: ReconcileContext,
        module: &ResolvedModule,
    ) -> Self {
        Self {
            script_dir: module.dir.clone(),
            env: build_module_script_env(
                config_dir,
                profile_name,
                context,
                &ScriptPhase::Patch,
                Some(&module.name),
                Some(&module.dir),
                &module.env,
            ),
        }
    }

    /// Binding for a file whose owner is known only as an [`Origin`] — the
    /// effective-state view, where profile files and module files arrive in one
    /// list.
    ///
    /// An origin naming a module absent from `modules` is an error, not a
    /// fallback to the profile binding: that would anchor the script at the
    /// config directory instead of the module directory, and a relative
    /// `script:` that resolves nowhere is run as an inline command — the exact
    /// silent misbehaviour the binding exists to prevent.
    pub fn for_origin(
        config_dir: &Path,
        profile_name: &str,
        context: ReconcileContext,
        modules: &[ResolvedModule],
        origin: &Origin,
    ) -> Result<Self> {
        let name = match origin {
            Origin::Module(name) => name,
            Origin::Profile => return Ok(Self::profile(config_dir, profile_name, context)),
        };
        let module = modules
            .iter()
            .find(|m| &m.name == name)
            .ok_or_else(|| crate::errors::ModuleError::NotFound { name: name.clone() })?;
        Ok(Self::module(config_dir, profile_name, context, module))
    }

    /// Borrow the binding as an execution context for [`compute_patched`] /
    /// [`evaluate_patch`].
    pub fn context(&self) -> PatchContext<'_> {
        PatchContext::new(&self.script_dir).with_env(&self.env)
    }
}

/// A `Patch` target's content before and after the spec is folded in.
#[derive(Debug)]
pub struct PatchOutcome {
    /// The target's content on disk; empty when the target does not exist.
    pub current: String,
    /// The content the spec produces from `current`.
    pub patched: String,
}

impl PatchOutcome {
    /// Whether applying the spec would change the target — the single
    /// up-to-date predicate shared by plan, diff, drift, and compliance so they
    /// can never disagree about whether a `Patch` file has converged.
    pub fn is_up_to_date(&self) -> bool {
        self.current == self.patched
    }
}

/// Wording shared by every read-only surface that could not evaluate a `Patch`
/// spec, so `diff`, `verify`, `status` and a compliance snapshot describe the
/// same failure identically. Collapsed to one line because it lands in a status
/// subject or a compliance detail.
pub fn patch_failure_detail(error: &crate::errors::CfgdError) -> String {
    format!(
        "cannot evaluate patch spec: {}",
        crate::output::collapse_to_subject_line(error)
    )
}

/// Read `target` and compute what `spec` would make of it.
///
/// A missing target reads as empty content (`ensure` then creates a minimal
/// document, `script` receives empty stdin), matching [`compute_patched`]'s
/// contract. Any other read failure — a directory, a permission error, non-UTF-8
/// bytes — is surfaced rather than silently treated as empty, because writing
/// the merge result would then destroy content cfgd could not read.
pub fn evaluate_patch(
    spec: &PatchSpec,
    target: &Path,
    ctx: &PatchContext<'_>,
) -> Result<PatchOutcome> {
    let current = match std::fs::read_to_string(target) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(FileError::Io {
                path: target.to_path_buf(),
                source: e,
            }
            .into());
        }
    };
    let patched = compute_patched(&current, spec, target, ctx)?;
    Ok(PatchOutcome { current, patched })
}

/// Compute the new content of `target` by applying `spec` to `current`.
///
/// A missing target is passed in as an empty `current`: `ensure` then creates a
/// minimal document and `script` receives empty stdin. The function is pure
/// with respect to the filesystem for `ensure` mode — it never reads or writes
/// the target — so the same call produces both the plan preview and the applied
/// content.
pub fn compute_patched(
    current: &str,
    spec: &PatchSpec,
    target: &Path,
    ctx: &PatchContext<'_>,
) -> Result<String> {
    // The single chokepoint every evaluation path (plan, apply, diff, verify,
    // compliance) funnels through, so a poisoned spec cannot run anywhere.
    if let Some(source_name) = spec.blocked_by.as_deref() {
        return Err(FileError::PatchScriptBlocked {
            path: target.to_path_buf(),
            source_name: source_name.to_string(),
        }
        .into());
    }
    match (spec.ensure.as_ref(), spec.script.as_deref()) {
        (Some(ensure), None) => match resolve_format(spec, target)? {
            PatchFormat::Ini => merge_ini(current, ensure, target),
            PatchFormat::Json => merge_json(current, ensure, target),
            PatchFormat::Yaml => merge_yaml(current, ensure, target),
            PatchFormat::Toml => merge_toml(current, ensure, target),
        },
        (None, Some(script)) => run_script(current, script, target, ctx),
        _ => Err(FileError::PatchSpecInvalid {
            path: target.to_path_buf(),
        }
        .into()),
    }
}

/// Resolve the format to parse the target as: the explicit `patch.format`
/// when set, otherwise inferred from the target's extension.
pub fn resolve_format(spec: &PatchSpec, target: &Path) -> Result<PatchFormat> {
    match spec.format {
        Some(format) => Ok(format),
        None => infer_format(target).ok_or_else(|| {
            FileError::PatchFormatUnknown {
                path: target.to_path_buf(),
            }
            .into()
        }),
    }
}

/// Infer a [`PatchFormat`] from a target's extension. `None` for any other
/// extension (and for extensionless paths) — the caller turns that into a
/// typed error telling the author to set `patch.format`.
pub fn infer_format(target: &Path) -> Option<PatchFormat> {
    let ext = target.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "ini" => Some(PatchFormat::Ini),
        "json" => Some(PatchFormat::Json),
        "yaml" | "yml" => Some(PatchFormat::Yaml),
        "toml" => Some(PatchFormat::Toml),
        _ => None,
    }
}

fn format_label(format: PatchFormat) -> &'static str {
    match format {
        PatchFormat::Ini => "ini",
        PatchFormat::Json => "json",
        PatchFormat::Yaml => "yaml",
        PatchFormat::Toml => "toml",
    }
}

fn parse_error(target: &Path, format: PatchFormat, message: impl std::fmt::Display) -> FileError {
    FileError::PatchParse {
        path: target.to_path_buf(),
        format: format_label(format).to_string(),
        message: message.to_string(),
    }
}

fn serialize_error(
    target: &Path,
    format: PatchFormat,
    message: impl std::fmt::Display,
) -> FileError {
    FileError::PatchSerialize {
        path: target.to_path_buf(),
        format: format_label(format).to_string(),
        message: message.to_string(),
    }
}

fn shape_error(target: &Path, format: PatchFormat, message: impl Into<String>) -> FileError {
    FileError::PatchEnsureShape {
        path: target.to_path_buf(),
        format: format_label(format).to_string(),
        message: message.into(),
    }
}

/// `ensure` must be a mapping for every format: a scalar or list at the top
/// level would replace the entire document, which is the opposite of what the
/// `Patch` strategy promises.
fn ensure_mapping<'v>(
    ensure: &'v serde_yaml::Value,
    target: &Path,
    format: PatchFormat,
) -> Result<&'v serde_yaml::Mapping> {
    ensure
        .as_mapping()
        .ok_or_else(|| shape_error(target, format, "'ensure' must be a mapping of keys").into())
}

// ---------------------------------------------------------------------------
// YAML
// ---------------------------------------------------------------------------

fn merge_yaml(current: &str, ensure: &serde_yaml::Value, target: &Path) -> Result<String> {
    let overlay = ensure_mapping(ensure, target, PatchFormat::Yaml)?;
    let mut doc: serde_yaml::Value = if current.trim().is_empty() {
        serde_yaml::Value::Mapping(serde_yaml::Mapping::new())
    } else {
        serde_yaml::from_str::<serde_yaml::Value>(current)
            .map_err(|e| parse_error(target, PatchFormat::Yaml, e))?
    };
    // An all-comment document parses to Null; merging into it would drop the
    // ensured keys on the floor.
    if doc.is_null() {
        doc = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    }
    if !doc.is_mapping() {
        return Err(parse_error(
            target,
            PatchFormat::Yaml,
            "top-level document is not a mapping",
        )
        .into());
    }
    crate::deep_merge_yaml(&mut doc, &serde_yaml::Value::Mapping(overlay.clone()));
    serde_yaml::to_string(&doc).map_err(|e| serialize_error(target, PatchFormat::Yaml, e).into())
}

// ---------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------

/// Merge into JSON *through* `serde_yaml::Value`, not `serde_json::Value`.
///
/// `serde_json::Map` is a `BTreeMap` unless the workspace-wide `preserve_order`
/// feature is on, so a `serde_json::Value` round-trip would silently re-sort a
/// user's `settings.json` alphabetically. `serde_yaml::Mapping` is insertion-
/// ordered, and serializing it straight into `serde_json`'s writer (no
/// intermediate `Value`) emits the keys in document order. That keeps the
/// promise this strategy makes about leaving untouched content alone, reuses
/// the shared [`crate::deep_merge_yaml`], and costs the workspace nothing —
/// enabling `preserve_order` would have reordered every generated schema and
/// CRD in the repo.
fn merge_json(current: &str, ensure: &serde_yaml::Value, target: &Path) -> Result<String> {
    let overlay = ensure_mapping(ensure, target, PatchFormat::Json)?;
    let mut doc: serde_yaml::Value = if current.trim().is_empty() {
        serde_yaml::Value::Mapping(serde_yaml::Mapping::new())
    } else {
        parse_json_last_wins(current).map_err(|e| parse_error(target, PatchFormat::Json, e))?
    };
    if !doc.is_mapping() {
        return Err(parse_error(
            target,
            PatchFormat::Json,
            "top-level document is not an object",
        )
        .into());
    }
    let overlay = prepare_json_overlay(
        &serde_yaml::Value::Mapping(overlay.clone()),
        target,
        &mut String::new(),
    )?;
    crate::deep_merge_yaml(&mut doc, &overlay);
    let mut out = serde_json::to_string_pretty(&doc)
        .map_err(|e| serialize_error(target, PatchFormat::Json, e))?;
    out.push('\n');
    Ok(out)
}

/// Validate a JSON `ensure` overlay and normalize what JSON expresses
/// differently from YAML.
///
/// Three concerns share one walk because they share one cause — a value YAML can
/// hold that JSON cannot round-trip:
///
/// - a non-string key would be *written* as `"42"` and read back as a string
///   while the overlay still carries a number, so the pair would duplicate on
///   every reconcile;
/// - `.nan` / `.inf` serialize to `null`, silently writing a different value
///   than the spec asked for;
/// - a `!Tag` would emit `{"!Tag": v}` — an object the author never wrote. INI
///   and TOML unwrap tags, so JSON does too.
///
/// Sequences are walked as well: a mapping nested inside a list is just as
/// unable to round-trip as a top-level one.
fn prepare_json_overlay(
    value: &serde_yaml::Value,
    target: &Path,
    path: &mut String,
) -> Result<serde_yaml::Value> {
    Ok(match value {
        serde_yaml::Value::Mapping(map) => {
            let mut out = serde_yaml::Mapping::with_capacity(map.len());
            for (key, child) in map {
                let Some(name) = key.as_str() else {
                    return Err(shape_error(
                        target,
                        PatchFormat::Json,
                        format!(
                            "object keys must be strings, but {} has a non-string key: {}",
                            json_path_label(path),
                            yaml_key_label(key)
                        ),
                    )
                    .into());
                };
                let restore = path.len();
                path.push_str(name);
                path.push('.');
                let prepared = prepare_json_overlay(child, target, path)?;
                path.truncate(restore);
                out.insert(serde_yaml::Value::String(name.to_string()), prepared);
            }
            serde_yaml::Value::Mapping(out)
        }
        serde_yaml::Value::Sequence(items) => {
            let mut out = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                let restore = path.len();
                path.push_str(&format!("[{index}]."));
                out.push(prepare_json_overlay(item, target, path)?);
                path.truncate(restore);
            }
            serde_yaml::Value::Sequence(out)
        }
        serde_yaml::Value::Number(n) if n.is_f64() && !n.as_f64().is_some_and(f64::is_finite) => {
            return Err(shape_error(
                target,
                PatchFormat::Json,
                format!(
                    "{} is {n} — JSON has no NaN or Infinity",
                    json_path_label(path)
                ),
            )
            .into());
        }
        serde_yaml::Value::Tagged(tagged) => prepare_json_overlay(&tagged.value, target, path)?,
        other => other.clone(),
    })
}

/// Render the overlay path for an operator-facing message.
///
/// The buffer carries a trailing `.` so the walk can append a segment cheaply;
/// an error message must not show that separator dangling against whatever
/// follows it (`list.[1]..inf`).
fn json_path_label(path: &str) -> String {
    let trimmed = path.strip_suffix('.').unwrap_or(path);
    if trimmed.is_empty() {
        "the overlay root".to_string()
    } else {
        format!("'{trimmed}'")
    }
}

/// Render a mapping key the way the author wrote it in YAML, never as a Rust
/// `Debug` form (`Number(42)`) — the message is read by an operator fixing
/// their own config, not by someone reading cfgd's source.
fn yaml_key_label(key: &serde_yaml::Value) -> String {
    match key {
        serde_yaml::Value::Null => "null".to_string(),
        serde_yaml::Value::Bool(b) => b.to_string(),
        serde_yaml::Value::Number(n) => n.to_string(),
        serde_yaml::Value::String(s) => s.clone(),
        serde_yaml::Value::Sequence(_) => "a list".to_string(),
        serde_yaml::Value::Mapping(_) => "a mapping".to_string(),
        serde_yaml::Value::Tagged(tagged) => yaml_key_label(&tagged.value),
    }
}

/// Parse JSON text into an insertion-ordered [`serde_yaml::Value`], resolving a
/// repeated object key to its last occurrence.
///
/// A plain `serde_json::from_str::<serde_yaml::Value>` rejects duplicate keys
/// outright, which would turn a tolerable quirk of a user-owned file into a hard
/// failure of the whole apply. `serde_json`'s own default is last-wins, so this
/// keeps `Patch` as permissive as the parser the file is written for while
/// still preserving key order (`Mapping::insert` overwrites the value and keeps
/// the key's original position).
fn parse_json_last_wins(
    current: &str,
) -> std::result::Result<serde_yaml::Value, serde_json::Error> {
    serde_json::from_str::<LastWinsJson>(current).map(|parsed| parsed.0)
}

/// Deserialization shim for [`parse_json_last_wins`].
struct LastWinsJson(serde_yaml::Value);

impl<'de> serde::Deserialize<'de> for LastWinsJson {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(LastWinsJsonVisitor)
    }
}

struct LastWinsJsonVisitor;

impl<'de> serde::de::Visitor<'de> for LastWinsJsonVisitor {
    type Value = LastWinsJson;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_bool<E>(self, v: bool) -> std::result::Result<Self::Value, E> {
        Ok(LastWinsJson(serde_yaml::Value::Bool(v)))
    }

    fn visit_i64<E>(self, v: i64) -> std::result::Result<Self::Value, E> {
        Ok(LastWinsJson(serde_yaml::Value::Number(v.into())))
    }

    fn visit_u64<E>(self, v: u64) -> std::result::Result<Self::Value, E> {
        Ok(LastWinsJson(serde_yaml::Value::Number(v.into())))
    }

    fn visit_f64<E>(self, v: f64) -> std::result::Result<Self::Value, E> {
        Ok(LastWinsJson(serde_yaml::Value::Number(v.into())))
    }

    fn visit_str<E>(self, v: &str) -> std::result::Result<Self::Value, E> {
        Ok(LastWinsJson(serde_yaml::Value::String(v.to_string())))
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(LastWinsJson(serde_yaml::Value::Null))
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(LastWinsJson(serde_yaml::Value::Null))
    }

    fn visit_some<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(LastWinsJsonVisitor)
    }

    fn visit_seq<A>(self, mut seq: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: serde::de::SeqAccess<'de>,
    {
        let mut items = Vec::new();
        while let Some(LastWinsJson(item)) = seq.next_element()? {
            items.push(item);
        }
        Ok(LastWinsJson(serde_yaml::Value::Sequence(items)))
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: serde::de::MapAccess<'de>,
    {
        let mut out = serde_yaml::Mapping::new();
        while let Some((key, LastWinsJson(value))) = map.next_entry::<String, LastWinsJson>()? {
            out.insert(serde_yaml::Value::String(key), value);
        }
        Ok(LastWinsJson(serde_yaml::Value::Mapping(out)))
    }
}

// ---------------------------------------------------------------------------
// TOML
// ---------------------------------------------------------------------------

fn merge_toml(current: &str, ensure: &serde_yaml::Value, target: &Path) -> Result<String> {
    let overlay = ensure_mapping(ensure, target, PatchFormat::Toml)?;
    let mut doc: toml_edit::DocumentMut = if current.trim().is_empty() {
        toml_edit::DocumentMut::new()
    } else {
        current
            .parse()
            .map_err(|e| parse_error(target, PatchFormat::Toml, e))?
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
        PatchFormat::Toml,
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
                set_toml_value(table, key, new_value);
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

/// Assign a value into a table, carrying over the existing value's decor so an
/// updated key keeps its surrounding whitespace and trailing comment.
///
/// A key that previously held a *table* has no value decor to inherit, and the
/// key's own decor came from a `[header]` context, so the render would collapse
/// to `key= 5`. Those get an explicit `key = value` decor instead.
fn set_toml_value(table: &mut toml_edit::Table, key: &str, new_value: toml_edit::Value) {
    let previous = table.get(key);
    let inherited = previous
        .and_then(toml_edit::Item::as_value)
        .map(|old| old.decor().clone());
    let replacing_non_value = previous.is_some() && inherited.is_none();

    let mut replacement = new_value;
    if let Some(decor) = inherited {
        *replacement.decor_mut() = decor;
    } else if replacing_non_value {
        *replacement.decor_mut() = toml_edit::Decor::new(" ", "");
    }

    // Mutate the slot rather than re-inserting: `Table::insert` replaces the
    // stored `Key` too, and an own-line comment above the key lives in that
    // key's decor — re-inserting would silently delete it.
    match table.get_mut(key) {
        Some(item) => *item = toml_edit::Item::Value(replacement),
        None => {
            table.insert(key, toml_edit::Item::Value(replacement));
        }
    }

    if replacing_non_value && let Some(mut existing_key) = table.key_mut(key) {
        *existing_key.leaf_decor_mut() = toml_edit::Decor::new("", " ");
    }
}

fn toml_key<'k>(key: &'k serde_yaml::Value, target: &Path) -> Result<&'k str> {
    key.as_str()
        .ok_or_else(|| shape_error(target, PatchFormat::Toml, "table keys must be strings").into())
}

fn yaml_to_toml(value: &serde_yaml::Value, target: &Path) -> Result<toml_edit::Value> {
    Ok(match value {
        serde_yaml::Value::Bool(b) => toml_edit::Value::from(*b),
        serde_yaml::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                toml_edit::Value::from(i)
            } else if n.is_f64()
                && let Some(f) = n.as_f64()
            {
                toml_edit::Value::from(f)
            } else {
                // TOML integers are signed 64-bit. Falling through to `as_f64`
                // would silently round a `u64` above `i64::MAX`, writing a
                // different number than the spec asked for.
                return Err(shape_error(
                    target,
                    PatchFormat::Toml,
                    format!("{n} is outside TOML's signed 64-bit integer range"),
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
                PatchFormat::Toml,
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
    let overlay = ensure_mapping(ensure, target, PatchFormat::Ini)?;
    let mut doc = IniDoc::parse(current);
    for (key, value) in overlay {
        let name = key.as_str().ok_or_else(|| {
            shape_error(
                target,
                PatchFormat::Ini,
                "section and key names must be strings",
            )
        })?;
        match value {
            serde_yaml::Value::Mapping(section) => {
                validate_ini_name(name, IniNameKind::Section, target)?;
                let mut pairs = Vec::with_capacity(section.len());
                for (k, v) in section {
                    let k = k.as_str().ok_or_else(|| {
                        shape_error(target, PatchFormat::Ini, "key names must be strings")
                    })?;
                    validate_ini_name(k, IniNameKind::Key, target)?;
                    pairs.push((k, ini_scalar(v, target, &format!("{name}.{k}"))?));
                }
                doc.set_section_keys(name, &pairs);
            }
            other => {
                validate_ini_name(name, IniNameKind::Key, target)?;
                let rendered = ini_scalar(other, target, name)?;
                doc.set_global_key(name, &rendered);
            }
        }
    }
    Ok(doc.render())
}

/// Characters that would make a rendered INI name unreadable by
/// [`ini_key_name`] / [`ini_section_name`] on the next pass.
const INI_NAME_FORBIDDEN: &[char] = &['\n', '\r', '=', '[', ']'];

/// Which reader has to be able to read the rendered name back.
#[derive(Clone, Copy)]
enum IniNameKind {
    Section,
    Key,
}

impl IniNameKind {
    fn label(self) -> &'static str {
        match self {
            IniNameKind::Section => "section name",
            IniNameKind::Key => "key name",
        }
    }
}

/// Reject a section or key name that cannot survive a write/read round-trip.
///
/// INI has no escape syntax: a name carrying a newline, `=`, or a bracket
/// renders a line the parser reads back as a *different* name (or as no key at
/// all), so the merge would never find it again and every reconcile would
/// append another copy — unbounded growth of a user-owned file with no error.
/// Padding is rejected for the same reason: the readers trim, so `" x"` would
/// never match the `" x = v"` line it just wrote.
fn validate_ini_name(name: &str, kind: IniNameKind, target: &Path) -> Result<()> {
    let what = kind.label();
    if name.is_empty() || name.trim() != name {
        return Err(shape_error(
            target,
            PatchFormat::Ini,
            format!("{what} '{name}' must not be empty or padded with whitespace"),
        )
        .into());
    }
    // A key line opening with a comment marker reads back as a comment, so the
    // merge would never see its own key again. Section names are unaffected:
    // `[#foo]` still parses as a header, so it round-trips.
    if matches!(kind, IniNameKind::Key) && name.starts_with(['#', ';']) {
        return Err(shape_error(
            target,
            PatchFormat::Ini,
            format!(
                "{what} '{name}' starts with a comment marker — the line would read back as a comment"
            ),
        )
        .into());
    }
    if let Some(bad) = name.chars().find(|c| INI_NAME_FORBIDDEN.contains(c)) {
        return Err(shape_error(
            target,
            PatchFormat::Ini,
            format!(
                "{what} '{}' contains {} — INI has no escape syntax for it",
                name.escape_debug(),
                describe_ini_char(bad)
            ),
        )
        .into());
    }
    Ok(())
}

fn describe_ini_char(c: char) -> String {
    match c {
        '\n' => "a newline".to_string(),
        '\r' => "a carriage return".to_string(),
        other => format!("'{other}'"),
    }
}

/// Render an `ensure` value as an INI value. INI has no native list or nested
/// mapping syntax, so those are rejected rather than guessed at.
fn ini_scalar(value: &serde_yaml::Value, target: &Path, key: &str) -> Result<String> {
    match value {
        serde_yaml::Value::Bool(b) => Ok(b.to_string()),
        serde_yaml::Value::Number(n) => Ok(n.to_string()),
        serde_yaml::Value::String(s) => {
            // A value carrying a line break would render extra lines that the
            // next pass reads as unrelated content (or as a bogus `[section]`),
            // so the key would be re-appended on every reconcile.
            if let Some(bad) = s.chars().find(|c| *c == '\n' || *c == '\r') {
                return Err(shape_error(
                    target,
                    PatchFormat::Ini,
                    format!(
                        "value for '{key}' contains {} — an INI value is a single line",
                        describe_ini_char(bad)
                    ),
                )
                .into());
            }
            Ok(s.clone())
        }
        serde_yaml::Value::Tagged(tagged) => ini_scalar(&tagged.value, target, key),
        serde_yaml::Value::Null => Err(shape_error(
            target,
            PatchFormat::Ini,
            format!("'{key}' has no value — INI cannot express null"),
        )
        .into()),
        serde_yaml::Value::Sequence(_) => Err(shape_error(
            target,
            PatchFormat::Ini,
            format!("'{key}' is a list — INI supports section → key → scalar only"),
        )
        .into()),
        serde_yaml::Value::Mapping(_) => Err(shape_error(
            target,
            PatchFormat::Ini,
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

    /// Half-open line ranges of every block declared under `name`, in document
    /// order, each excluding its own header line.
    ///
    /// A file may repeat a header (`git config` and `systemd` both allow it and
    /// read the LAST value); editing only the first block would leave a later
    /// duplicate overriding the ensured value while cfgd reported success.
    fn section_ranges(&self, name: &str) -> Vec<(usize, usize)> {
        let headers: Vec<usize> = (0..self.lines.len())
            .filter(|idx| ini_section_name(&self.lines[*idx]).is_some())
            .collect();
        let mut ranges = Vec::new();
        for (position, &header) in headers.iter().enumerate() {
            if ini_section_name(&self.lines[header]) != Some(name) {
                continue;
            }
            let end = headers
                .get(position + 1)
                .copied()
                .unwrap_or(self.lines.len());
            ranges.push((header + 1, end));
        }
        ranges
    }

    fn set_global_key(&mut self, key: &str, value: &str) {
        let (start, end) = self.global_range();
        if self.update_in_range(start, end, key, value) {
            return;
        }
        self.normalize_blank_document();
        let (start, end) = self.global_range();
        let line = self.render_key_line(start, end, key, value);
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
        for (key, value) in pairs {
            // Recomputed per key: an insert shifts every later line index, and
            // `append_section` on the first key creates the block the rest land in.
            let ranges = self.section_ranges(section);
            let Some(&(last_start, last_end)) = ranges.last() else {
                self.append_section(section, key, value);
                continue;
            };

            let mut found = false;
            for (start, end) in ranges {
                if self.update_in_range(start, end, key, value) {
                    found = true;
                }
            }
            if found {
                continue;
            }

            // Insert into the LAST block for the name so last-wins parsers see
            // the ensured value; first-wins parsers see it too, as it is then
            // the only occurrence.
            let line = self.render_key_line(last_start, last_end, key, value);
            let at = match self.last_key_line(last_start, last_end) {
                Some(idx) => idx + 1,
                None => last_start,
            };
            self.lines.insert(at, line);
            self.trailing_newline = true;
        }
    }

    fn append_section(&mut self, section: &str, key: &str, value: &str) {
        self.normalize_blank_document();
        let nl = self.newline_suffix();
        if !self.lines.is_empty() {
            self.lines.push(nl.to_string());
        }
        self.lines.push(format!("[{section}]{nl}"));
        let line = self.render_key_line(0, self.lines.len(), key, value);
        self.lines.push(line);
        self.trailing_newline = true;
    }

    /// A whitespace-only target is an empty document, not one with blank lines
    /// worth preserving — keeping them would prefix the file with a blank line.
    fn normalize_blank_document(&mut self) {
        if self.lines.iter().all(|l| l.trim().is_empty()) {
            self.lines.clear();
        }
    }

    /// Render a new `key = value` line in the surrounding block's own style:
    /// its leading indentation and its spacing around `=`.
    fn render_key_line(&self, start: usize, end: usize, key: &str, value: &str) -> String {
        let indent = self
            .sampled(start, end, ini_line_indent)
            .unwrap_or_default();
        let sep = self
            .sampled(start, end, ini_separator_style)
            .unwrap_or_else(|| " = ".to_string());
        format!("{indent}{key}{sep}{value}{}", self.newline_suffix())
    }

    /// Read a style property off the first `key = value` line in the range,
    /// falling back to the first one anywhere in the document.
    fn sampled(&self, start: usize, end: usize, extract: fn(&str) -> String) -> Option<String> {
        let first_key_line = |range: std::ops::Range<usize>| {
            range
                .into_iter()
                .find(|idx| self.lines.get(*idx).and_then(|l| ini_key_name(l)).is_some())
        };
        first_key_line(start..end.min(self.lines.len()))
            .or_else(|| first_key_line(0..self.lines.len()))
            .map(|idx| extract(&self.lines[idx]))
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
}

/// `" = "` or `"="`, whichever the sampled line uses.
fn ini_separator_style(line: &str) -> String {
    let line = line.trim_end_matches('\r');
    match line.find('=') {
        Some(eq) if line[..eq].ends_with(' ') => " = ".to_string(),
        Some(_) => "=".to_string(),
        None => " = ".to_string(),
    }
}

/// The sampled line's leading whitespace, so a new key in a tab-indented
/// `.gitconfig` keeps the file's indentation instead of hugging the margin.
fn ini_line_indent(line: &str) -> String {
    line.chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

fn is_comment_or_blank(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';')
}

/// The section name of a `[name]` header line, or `None` for any other line.
///
/// A trailing `; comment` / `# comment` after the `]` is part of the header, not
/// a reason to stop recognizing it — missing that would make cfgd append a
/// duplicate `[name]` block instead of editing the one already there. The
/// header line itself is never rewritten, so the comment survives untouched.
fn ini_section_name(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    let rest = trimmed.strip_prefix('[')?;
    let close = rest.find(']')?;
    let tail = rest[close + 1..].trim();
    if !tail.is_empty() && !tail.starts_with(';') && !tail.starts_with('#') {
        return None;
    }
    Some(rest[..close].trim())
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
    ctx: &PatchContext<'_>,
) -> Result<String> {
    let default_workdir: PathBuf;
    let working_dir = match ctx.working_dir {
        Some(dir) => dir,
        None => {
            default_workdir = script_default_workdir(ctx.script_dir);
            &default_workdir
        }
    };

    let failed = |message: String| -> crate::errors::CfgdError {
        FileError::PatchScriptFailed {
            path: target.to_path_buf(),
            script: script.to_string(),
            message,
        }
        .into()
    };

    // Every way the filter can fail lands in the same typed variant, so the
    // error always names the target being patched — a bare `Io`/`Config` error
    // from the executor would say which *script* broke but not which file the
    // operator was trying to change.
    let outcome = run_filter_script(
        script,
        ctx.script_dir,
        working_dir,
        ctx.env,
        current,
        ctx.timeout,
    )
    .map_err(|e| failed(e.to_string()))?;

    if outcome.timed_out {
        return Err(failed(format!(
            "timed out after {}s",
            ctx.timeout.as_secs()
        )));
    }
    if !outcome.success {
        let exit = match outcome.exit_code {
            Some(code) => format!("exit {code}"),
            None => "killed by signal".to_string(),
        };
        return Err(failed(if outcome.stderr.is_empty() {
            exit
        } else {
            format!("{exit}: {}", outcome.stderr)
        }));
    }
    Ok(outcome.stdout)
}

#[cfg(test)]
mod tests;
