use super::*;
use cfgd_core::PathDisplayExt;
use cfgd_core::output::{Doc, Printer, Role};
use cfgd_core::yes_no;

// --- Config CRUD ---

pub fn build_config_show_doc(cfg: &CfgdConfig, config_path: &Path) -> Doc {
    let mut doc = Doc::new()
        .heading("Configuration")
        .kv(
            "File",
            cfgd_core::fold_home_in_text(&config_path.display_posix()),
        )
        .kv(
            "Profile",
            cfg.spec.profile.as_deref().unwrap_or("(none)").to_string(),
        );

    doc = doc.section_if_nonempty("Origins", &cfg.spec.origin, |s, origins| {
        origins.iter().enumerate().fold(s, |s, (i, origin)| {
            let label = if i == 0 { "Primary" } else { "Secondary" };
            s.subsection(label, |sub| {
                sub.kv("Url", &origin.url)
                    .kv("Type", format!("{:?}", origin.origin_type))
                    .kv("Branch", &origin.branch)
            })
        })
    });

    doc = doc.section_if_nonempty(
        super::source::list::SOURCES_SECTION,
        &cfg.spec.sources,
        |s, sources| {
            sources
                .iter()
                .fold(s, |s, src| s.kv(&src.name, &src.origin.url))
        },
    );

    if let Some(ref mods) = cfg.spec.modules {
        doc = doc.section_if_nonempty("Module Registries", &mods.registries, |s, regs| {
            regs.iter().fold(s, |s, ms| s.kv(&ms.name, &ms.url))
        });

        if let Some(ref sec) = mods.security {
            doc = doc.section("Module Security", |s| {
                s.kv("Require Signatures", yes_no(Some(sec.require_signatures)))
            });
        }
    }

    if let Some(ref daemon) = cfg.spec.daemon {
        doc = doc.section("Daemon", |s| {
            let mut s = s.kv("Enabled", yes_no(Some(daemon.enabled)));
            if let Some(ref reconcile) = daemon.reconcile {
                s = s.subsection("Reconcile", |sub| {
                    sub.kv("Interval", &reconcile.interval)
                        .kv("On Change", yes_no(Some(reconcile.on_change)))
                        .kv("Auto Apply", yes_no(Some(reconcile.auto_apply)))
                });
            }
            if let Some(ref sync) = daemon.sync {
                s = s.subsection("Sync", |sub| sub.kv("Interval", &sync.interval));
            }
            s
        });
    }

    if let Some(ref secrets) = cfg.spec.secrets {
        doc = doc.section("Secrets", |s| s.kv("Backend", &secrets.backend));
    }

    if let Some(ref output) = cfg.spec.output {
        let rows: Vec<cfgd_core::output::KvPair> = [
            output
                .theme
                .as_ref()
                .map(|t| cfgd_core::output::KvPair::new("Theme", t.name.clone())),
            output
                .usage_hints
                .map(|h| cfgd_core::output::KvPair::new("Usage Hints", yes_no(Some(h)))),
            output
                .mask_env_values
                .map(|m| cfgd_core::output::KvPair::new("Mask Env Values", m.as_str().to_string())),
        ]
        .into_iter()
        .flatten()
        .collect();
        if !rows.is_empty() {
            doc = doc.section("Output", |s| s.kv_rows(rows));
        }
    }

    doc.with_data(cfg)
}

pub fn cmd_config_show(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    let config_path = &cli.config;
    if !config_path.exists() {
        return Err(no_config_error(printer, config_path));
    }

    let cfg = match config::load_config(config_path) {
        Ok(mut c) => {
            drain_config_deprecations(printer, &mut c);
            c
        }
        Err(e) => {
            let msg = format!("{}", e);
            return Err(crate::cli::cli_error_ctx(
                e.into(),
                cfgd_core::to_posix_string(config_path),
                "parse_failed",
                msg,
                serde_json::json!({ "path": cfgd_core::to_posix_string(config_path) }),
            ));
        }
    };
    printer.emit(build_config_show_doc(&cfg, config_path));
    Ok(())
}

pub fn cmd_config_edit(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    let config_path = &cli.config;
    if !config_path.exists() {
        return Err(no_config_error(printer, config_path));
    }

    open_in_editor(config_path, printer)?;

    // Validate after editing — loop until valid or user cancels
    let mut valid = false;
    loop {
        match config::load_config(config_path) {
            Ok(mut c) => {
                drain_config_deprecations(printer, &mut c);
                valid = true;
                break;
            }
            Err(e) => {
                printer.status_simple(
                    // no-next-step: the prompt below IS the next step
                    Role::Fail,
                    format!(
                        "Invalid configuration: {}",
                        cfgd_core::output::collapse_to_subject_line(&e),
                    ),
                );
                if !printer.prompt_confirm("Re-open in editor to fix?")? {
                    break;
                }
                open_in_editor(config_path, printer)?;
            }
        }
    }

    if valid {
        printer.emit(
            Doc::new()
                // verdict-row-ok: a validation verdict, not an act cfgd performed
                .status(Role::Ok, "Configuration is valid")
                .with_data(serde_json::json!({
                    "path": cfgd_core::to_posix_string(config_path),
                    "valid": true,
                })),
        );
    } else {
        printer.emit(
            Doc::new()
                .status(Role::Warn, "Saved with validation errors")
                .with_data(serde_json::json!({
                    "path": cfgd_core::to_posix_string(config_path),
                    "valid": false,
                })),
        );
    }

    Ok(())
}

// --- Config get/set/unset ---

/// The `spec`-relative paths whose value is a scalar-or-mapping union, paired
/// with the field a bare scalar there stands for. Both rows are `ThemeConfig`:
/// `theme: dracula` IS `theme: {name: dracula}`, and that scalar arm is what
/// `cfgd init` and `cfgd config set theme <name>` write, so a walk refusing to
/// descend through it would fail every documented `config set theme.name` on
/// cfgd's own document. The legacy flat spelling is listed because `get` falls
/// back to it on an unmigrated document.
const SCALAR_UNION_FIELDS: &[(&str, &str)] = &[("output.theme", "name"), ("theme", "name")];

/// The field a bare scalar at these path segments stands for, or `None` where a
/// scalar is genuinely a leaf.
pub(super) fn scalar_union_field(segments: &[&str]) -> Option<&'static str> {
    SCALAR_UNION_FIELDS.iter().find_map(|(path, field)| {
        path.split('.')
            .eq(segments.iter().copied())
            .then_some(*field)
    })
}

/// Whether this value is a scalar a union's mapping arm could have been
/// written as. A sequence is no arm of any union here, so it stays a shape
/// error.
pub(super) fn is_union_scalar(value: &serde_yaml::Value) -> bool {
    blocking_shape(value) == SHAPE_SCALAR
}

/// The words a refusal calls the two shapes whose refusal turns on what the
/// schema declares at the path they blocked.
const SHAPE_SCALAR: &str = "a scalar";
pub(in crate::cli) const SHAPE_SEQUENCE: &str = "a sequence";
pub(in crate::cli) const SHAPE_MAPPING: &str = "a mapping";

/// What a value that blocked a descent IS, as the refusal words it. Captured
/// before the parent it sits in is borrowed mutably, so both walkers can name
/// the shape they found.
pub(in crate::cli) fn blocking_shape(value: &serde_yaml::Value) -> &'static str {
    match value {
        serde_yaml::Value::String(_)
        | serde_yaml::Value::Number(_)
        | serde_yaml::Value::Bool(_) => SHAPE_SCALAR,
        serde_yaml::Value::Sequence(_) => SHAPE_SEQUENCE,
        serde_yaml::Value::Mapping(_) => SHAPE_MAPPING,
        serde_yaml::Value::Tagged(_) => "a tagged value",
        serde_yaml::Value::Null => "nothing",
    }
}

/// A descent blocked by a value that is no mapping and no union arm cfgd can
/// promote. Typed, so the classifier reads the failure the walker actually hit
/// with no match on a message four situations shared.
#[derive(Debug)]
pub(super) struct ShapeBlocked {
    /// The key path that blocked, or `None` for the document itself, which no
    /// key names.
    path: Option<String>,
    found: &'static str,
    wanted: &'static str,
}

impl std::fmt::Display for ShapeBlocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.path {
            Some(path) => write!(
                f,
                "'{path}' holds {} where {} belongs",
                self.found, self.wanted
            ),
            None => write!(
                f,
                "the config document holds {} where {} belongs",
                self.found, self.wanted
            ),
        }
    }
}

impl std::error::Error for ShapeBlocked {}

/// The typed missing-key refusal both walkers mint, naming the path that is
/// not there.
fn key_not_found(asked: &[&str]) -> anyhow::Error {
    anyhow::Error::new(cfgd_core::errors::CfgdError::Config(
        cfgd_core::errors::ConfigError::KeyNotFound {
            key: asked.join("."),
        },
    ))
}

/// A key walk's refusal on `resolved`, restated for the key the caller wrote as
/// `asked`, which [`nested_output_key`] rewrote into `resolved`.
///
/// A missing key is reported under `asked`: the first segment the walk found
/// missing belongs to the rewritten path, which the caller never typed. A
/// document whose shape blocks the walk keeps the path that holds the wrong
/// shape, since that path is where the reader has to look, and adds where the
/// key they asked for lives. Every other refusal is about the document or the
/// file and names no key, so it passes through as it is.
fn as_asked(error: anyhow::Error, asked: &str, resolved: &str) -> anyhow::Error {
    if asked == resolved {
        return error;
    }
    if matches!(
        error.downcast_ref::<cfgd_core::errors::CfgdError>(),
        Some(cfgd_core::errors::CfgdError::Config(
            cfgd_core::errors::ConfigError::KeyNotFound { .. }
        ))
    ) {
        return key_not_found(&[asked]);
    }
    if error.downcast_ref::<ShapeBlocked>().is_some() {
        let message = format!("{error}; '{asked}' is stored at '{resolved}'");
        return error.context(message);
    }
    error
}

/// The value the typed config holds at the `spec`-relative `key`, as the
/// build reads it: the typed value's own serialization carries every serde
/// default, so a key the document leaves out still has one. `None` where the
/// typed value has no such key.
fn typed_value_at(spec: &cfgd_core::config::ConfigSpec, key: &str) -> Option<serde_yaml::Value> {
    let tree = serde_yaml::to_value(spec).ok()?;
    walk_yaml_path(&tree, key).ok().cloned()
}

/// The refusal a key path holding an empty segment (`a..b`, a trailing `.`)
/// earns: no key is spelled that way, so the path itself is the bad input.
fn empty_segment(path: &str) -> anyhow::Error {
    crate::cli::cli_error(
        path,
        "invalid_value",
        format!("invalid key path '{path}': contains empty segment"),
        serde_json::json!({}),
    )
}

/// The mapping a section of a config document holds, where a section holding
/// nothing becomes an empty one; `None` for any other shape. A bare `key:` and
/// a serialized `None` both read back as Null, and both mean the section is
/// not there yet, so a writer creates it.
pub(in crate::cli) fn section_mapping_mut(
    value: &mut serde_yaml::Value,
) -> Option<&mut serde_yaml::Mapping> {
    if value.is_null() {
        *value = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    }
    // section-write-ok: the section rule itself
    value.as_mapping_mut()
}

/// [`section_mapping_mut`]'s twin for a section the schema declares a list.
pub(in crate::cli) fn section_sequence_mut(
    value: &mut serde_yaml::Value,
) -> Option<&mut serde_yaml::Sequence> {
    if value.is_null() {
        *value = serde_yaml::Value::Sequence(Vec::new());
    }
    // section-write-ok: the section rule itself
    value.as_sequence_mut()
}

/// A config document's `spec`, as the mapping a verb writing under it edits.
///
/// An absent `spec` and a bare `spec:` are an empty mapping, the rule
/// [`section_mapping_mut`] applies to every section the key walker descends
/// through; any other shape is refused `parse_failed`, naming what the
/// document holds there. The refusal carries [`ShapeBlocked`] as its source,
/// so `config set` and `config unset` classify it as they classify the same
/// block met further down a key path.
pub(in crate::cli) fn spec_mapping_mut<'a>(
    root: &'a mut serde_yaml::Value,
    config_path: &Path,
) -> anyhow::Result<&'a mut serde_yaml::Mapping> {
    let found = blocking_shape(root);
    let document = section_mapping_mut(root).ok_or_else(|| {
        shape_refusal(
            config_path,
            ShapeBlocked {
                path: None,
                found,
                wanted: SHAPE_MAPPING,
            },
        )
    })?;
    let spec = document
        .entry(serde_yaml::Value::String("spec".into()))
        .or_insert(serde_yaml::Value::Null);
    let found = blocking_shape(spec);
    section_mapping_mut(spec)
        .ok_or_else(|| section_shape_refusal(config_path, "spec", found, SHAPE_MAPPING))
}

/// The `parse_failed` refusal a writer earns from a section of the config
/// document at `path` holding `found` where the schema declares `wanted`.
/// Its source is the same [`ShapeBlocked`] a blocked key walk mints, so the
/// wording and the classification are one whichever writer met the block.
pub(in crate::cli) fn section_shape_refusal(
    config_path: &Path,
    path: &str,
    found: &'static str,
    wanted: &'static str,
) -> anyhow::Error {
    shape_refusal(
        config_path,
        ShapeBlocked {
            path: Some(path.to_string()),
            found,
            wanted,
        },
    )
}

fn shape_refusal(config_path: &Path, blocked: ShapeBlocked) -> anyhow::Error {
    let message = blocked.to_string();
    let file = cfgd_core::to_posix_string(config_path);
    let extras = serde_json::json!({ "path": &file });
    crate::cli::cli_error_ctx(
        anyhow::Error::new(blocked),
        file,
        "parse_failed",
        message,
        extras,
    )
}

/// The refusal a descent blocked at `path` earns, where `asked` is the path
/// the caller named and `found` the shape that blocked it.
///
/// Which one it is turns on the shape the schema declares at `path`, on both
/// branches. A child of a genuine scalar leaf, and a key named under a
/// declared list, can never exist however the document is written: the key
/// walkers address no sequence element, so neither says anything is wrong with
/// the document, and both are the missing key the absent-section arm answers
/// with. Every other block — a scalar where the schema declares a mapping or a
/// list, a sequence where it declares a mapping or a value — is a document
/// whose shape contradicts the schema, which a script must be able to tell
/// from a key it can simply create.
fn descent_blocked(path: &[&str], asked: &[&str], found: &'static str) -> anyhow::Error {
    use super::explain::DeclaredShape;

    let declared = if path.is_empty() {
        DeclaredShape::Mapping
    } else {
        super::explain::config_field_shape(path)
    };
    let document_agrees_with_schema = matches!(
        (found, declared),
        (SHAPE_SCALAR, DeclaredShape::Leaf | DeclaredShape::Unknown)
            | (SHAPE_SEQUENCE, DeclaredShape::Sequence)
    );
    if document_agrees_with_schema {
        return key_not_found(asked);
    }
    anyhow::Error::new(ShapeBlocked {
        // The root of the walk is `spec` itself, which every path is relative
        // to and no segment names, so an empty path is the mapping the whole
        // field list hangs off.
        path: Some(if path.is_empty() {
            "spec".to_string()
        } else {
            path.join(".")
        }),
        found,
        wanted: SHAPE_MAPPING,
    })
}

/// Rewrite a union's scalar arm in place as the mapping it stands for, so a
/// write to a field beneath it descends.
fn promote_scalar_union(value: &mut serde_yaml::Value, segments: &[&str]) {
    let Some(field) = scalar_union_field(segments) else {
        return;
    };
    if !is_union_scalar(value) {
        return;
    }
    let mut promoted = serde_yaml::Mapping::new();
    promoted.insert(
        serde_yaml::Value::String(field.to_string()),
        std::mem::replace(value, serde_yaml::Value::Null),
    );
    *value = serde_yaml::Value::Mapping(promoted);
}

/// Walk a dotted key path through a YAML value, returning the leaf.
/// Use "." to return the root value itself.
pub(super) fn walk_yaml_path<'a>(
    value: &'a serde_yaml::Value,
    path: &str,
) -> anyhow::Result<&'a serde_yaml::Value> {
    if path == "." {
        return Ok(value);
    }
    let segments: Vec<&str> = path.split('.').collect();
    if segments.iter().any(|s| s.is_empty()) {
        return Err(empty_segment(path));
    }
    let mut current = value;

    for (i, segment) in segments.iter().enumerate() {
        match current {
            serde_yaml::Value::Mapping(map) => {
                let key = serde_yaml::Value::String((*segment).to_string());
                current = map
                    .get(&key)
                    .ok_or_else(|| key_not_found(&segments[..=i]))?;
            }
            // `daemon:` with nothing beneath it parses as Null and means the
            // section is absent, so a key asked for under it is not found —
            // only a value standing in the way is a shape error.
            serde_yaml::Value::Null => {
                return Err(key_not_found(&segments[..=i]));
            }
            other => {
                // A union's scalar arm is its mapping with one field set, so
                // that field answers from the scalar itself and every other
                // field of the arm is absent.
                if is_union_scalar(other)
                    && let Some(field) = scalar_union_field(&segments[..i])
                {
                    if *segment == field && i + 1 == segments.len() {
                        return Ok(other);
                    }
                    return Err(key_not_found(&segments[..=i]));
                }
                return Err(descent_blocked(
                    &segments[..i],
                    &segments[..=i],
                    blocking_shape(other),
                ));
            }
        }
    }

    Ok(current)
}

/// Walk a dotted key path, creating intermediate mappings as needed.
/// Returns a mutable reference to the *parent* mapping and the leaf key name.
pub(super) fn walk_yaml_path_mut<'a>(
    value: &'a mut serde_yaml::Value,
    path: &str,
) -> anyhow::Result<(&'a mut serde_yaml::Mapping, String)> {
    let segments = key_segments(path)?;
    let found = blocking_shape(value);
    let root =
        section_mapping_mut(value).ok_or_else(|| descent_blocked(&[], &segments[..1], found))?;
    walk_spec_path_mut(root, path)
}

/// [`walk_yaml_path_mut`] from a `spec` already in hand as a mapping, the
/// shape [`spec_mapping_mut`] hands a writer.
pub(super) fn walk_spec_path_mut<'a>(
    spec: &'a mut serde_yaml::Mapping,
    path: &str,
) -> anyhow::Result<(&'a mut serde_yaml::Mapping, String)> {
    let segments = key_segments(path)?;
    let (leaf, parents) = segments
        .split_last()
        // untyped-ok: `split` yields at least one segment, so no input reaches this.
        .ok_or_else(|| anyhow::anyhow!("empty key path"))?;

    let mut parent = spec;
    // An absent key is inserted as Null, which reads as the empty section it
    // stands for, exactly as a `daemon:` holding nothing does.
    for (i, segment) in parents.iter().enumerate() {
        let slot = parent
            .entry(serde_yaml::Value::String((*segment).to_string()))
            .or_insert(serde_yaml::Value::Null);
        let at = &segments[..=i];
        promote_scalar_union(slot, at);
        let found = blocking_shape(slot);
        parent = section_mapping_mut(slot)
            .ok_or_else(|| descent_blocked(at, &segments[..i + 2], found))?;
    }
    Ok((parent, (*leaf).to_string()))
}

/// A key path's dot-separated segments, refused when one of them is empty.
fn key_segments(path: &str) -> anyhow::Result<Vec<&str>> {
    let segments: Vec<&str> = path.split('.').collect();
    if segments.iter().any(|s| s.is_empty()) {
        return Err(empty_segment(path));
    }
    Ok(segments)
}

/// Parse a string value into the most appropriate YAML type.
pub(super) fn parse_yaml_value(s: &str) -> serde_yaml::Value {
    match s {
        "true" => serde_yaml::Value::Bool(true),
        "false" => serde_yaml::Value::Bool(false),
        "null" | "~" => serde_yaml::Value::Null,
        _ => {
            // Try integer, then float, then string
            if let Ok(n) = s.parse::<i64>() {
                serde_yaml::Value::Number(n.into())
            } else if let Ok(f) = s.parse::<f64>() {
                serde_yaml::Value::Number(serde_yaml::Number::from(f))
            } else {
                serde_yaml::Value::String(s.to_string())
            }
        }
    }
}

/// A caller-written config key as this module addresses it: relative to
/// `spec`, which every path descends from and no segment names, so the
/// `spec.` prefix the docs and `cfgd explain` print is optional.
pub(super) fn spec_relative_key(key: &str) -> &str {
    key.strip_prefix("spec.").unwrap_or(key)
}

/// Resolve a `spec`-relative key path onto the nested `spec.output.*` key that
/// owns it, so `theme.name` and `output.theme.name` name one field.
///
/// `None` for every other path. The legacy flat spelling still names a real
/// key in a document that has not been migrated, which is why `get` falls back
/// to it and `set` removes it once it has written the nested one.
pub(super) fn nested_output_key(key: &str) -> Option<String> {
    // A path no field is spelled as is refused by the walk under the spelling
    // the caller wrote, so it is never rewritten first.
    if key.split('.').any(str::is_empty) {
        return None;
    }
    let (head, rest) = match key.split_once('.') {
        Some((head, rest)) => (head, Some(rest)),
        None => (key, None),
    };
    let nested = cfgd_core::config::LEGACY_OUTPUT_KEYS
        .iter()
        .find(|(old, _)| old.strip_prefix("spec.") == Some(head))
        .map(|(_, new)| new.trim_start_matches("spec."))?;
    Some(match rest {
        Some(rest) => format!("{nested}.{rest}"),
        None => nested.to_string(),
    })
}

/// The inverse: the flat spelling a `spec`-relative `output.*` key replaced,
/// so a `get` naming the current key still answers from a document that has
/// not been migrated. `None` for every other path.
pub(super) fn flat_output_key(key: &str) -> Option<String> {
    let (old, new) = cfgd_core::config::LEGACY_OUTPUT_KEYS
        .iter()
        .find_map(|(old, new)| {
            let new = new.trim_start_matches("spec.");
            (key == new || key.strip_prefix(new)?.starts_with('.'))
                .then_some((old.trim_start_matches("spec."), new))
        })?;
    Some(format!("{old}{}", &key[new.len()..]))
}

pub fn cmd_config_get(cli: &Cli, printer: &Printer, key: &str) -> anyhow::Result<()> {
    // The `spec.` prefix the docs and `cfgd explain` print is folded away
    // first, so every later read of the key — the walk, the confirmation, the
    // `-o json` payload and the error — names one field.
    let key = spec_relative_key(key);
    let config_path = &cli.config;
    if !config_path.exists() {
        return Err(no_config_error(printer, config_path));
    }

    let contents = std::fs::read_to_string(config_path)?;
    let raw = match crate::cli::source::config_tree(&contents, config_path) {
        Ok(v) => v,
        Err(e) => {
            let msg = format!("failed to parse config: {}", e);
            return Err(crate::cli::cli_error_ctx(
                e,
                key,
                "parse_failed",
                msg,
                serde_json::json!({ "path": cfgd_core::to_posix_string(config_path) }),
            ));
        }
    };

    let spec = match raw.get("spec") {
        Some(s) => s,
        None => {
            return Err(crate::cli::cli_error(
                key,
                "parse_failed",
                "config has no 'spec' section",
                serde_json::json!({ "path": cfgd_core::to_posix_string(config_path) }),
            ));
        }
    };

    // A legacy flat key names the nested one; a document that still carries
    // the flat spelling is answered from it rather than reported missing.
    let resolved = nested_output_key(key).unwrap_or_else(|| key.to_string());
    // The fallback is the flat twin of the key the walk RESOLVED to: a caller
    // writing the legacy spelling already resolves to the nested key, and
    // asking for its own twin again would leave `theme.name` — the spelling the
    // docs print — with no fallback at all on a document nothing has migrated.
    let alias = flat_output_key(&resolved);
    let walked = walk_yaml_path(spec, &resolved).or_else(|e| match alias.as_deref() {
        // Only a key that is not there is worth asking the other spelling
        // about: a shape the walk refused is a fact about the document, and
        // the legacy path would answer for it with a missing key.
        Some(alias) if alias != resolved && classify_config_error(&e) == "key_not_found" => {
            // A rescue that fails is not the refusal a reader gets: the key
            // they named is the resolved one, and its own walk said why.
            walk_yaml_path(spec, alias).map_err(|_| e)
        }
        _ => Err(e),
    });
    // A key the document leaves out still has the value this build reads for
    // it, so it is answered from the typed config. A key the schema does not
    // know, or whose typed value is null (an unset optional, a section left
    // out), stays the missing key the walk reported. A document that does not
    // parse as a config has no typed value, and keeps that refusal too.
    let typed_default = match &walked {
        Err(e) if classify_config_error(e) == "key_not_found" => {
            cfgd_core::config::parse_config(&contents, config_path)
                .ok()
                .and_then(|config| typed_value_at(&config.spec, &resolved))
                .filter(|value| !value.is_null())
        }
        _ => None,
    };
    let value = match (walked, typed_default.as_ref()) {
        (Ok(v), _) => v,
        (Err(_), Some(default)) => default,
        (Err(e), None) => {
            let e = as_asked(e, key, &resolved);
            let msg = format!("{}", e);
            let kind = classify_config_error(&e);
            return Err(crate::cli::cli_error_ctx(
                e,
                key,
                kind,
                msg,
                serde_json::json!({ "path": cfgd_core::to_posix_string(config_path) }),
            ));
        }
    };

    // human path writes the bare value via data_line; structured needs the keyed envelope.
    if printer.is_structured() {
        let json_value: serde_json::Value =
            serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
        printer.emit(Doc::new().with_data(serde_json::json!({
            "key": key,
            "value": json_value,
        })));
        return Ok(());
    }

    let rendered = match value {
        serde_yaml::Value::Null => String::new(),
        serde_yaml::Value::String(s) => s.clone(),
        serde_yaml::Value::Bool(b) => b.to_string(),
        serde_yaml::Value::Number(n) => n.to_string(),
        other => {
            let yaml = serde_yaml::to_string(other)?;
            yaml.strip_prefix("---\n")
                .unwrap_or(&yaml)
                .trim_end()
                .to_string()
        }
    };

    // Human output: bare value on stdout for piping (matches `git config <key>`,
    // `kubectl config view -o jsonpath` shape). Empty for null leaves.
    if !rendered.is_empty() {
        printer.data_line(&rendered);
    }

    Ok(())
}

pub fn cmd_config_set(cli: &Cli, printer: &Printer, key: &str, value: &str) -> anyhow::Result<()> {
    // The `spec.` prefix the docs and `cfgd explain` print is folded away
    // first, so every later read of the key — the walk, the confirmation, the
    // `-o json` payload and the error — names one field.
    let key = spec_relative_key(key);
    let config_path = &cli.config;
    if !config_path.exists() {
        return Err(no_config_error(printer, config_path));
    }

    let parsed_value = parse_yaml_value(value);
    let mut previous: serde_json::Value = serde_json::Value::Null;

    // A presentation knob is written where it now lives, whichever spelling
    // the caller reached for, and the flat key it replaced is dropped with it.
    let nested = nested_output_key(key);
    let written_key = nested.clone().unwrap_or_else(|| key.to_string());

    // The theme block's `name` is a free string in the document, so a name no
    // preset answers to would be stored and every later render would silently
    // fall back to the default palette. This setter runs with a printer in
    // hand, which is what `Theme::from_preset`'s render-time fallback does not,
    // so the refusal belongs here. Both spellings of the block are covered:
    // `output.theme` carrying a scalar IS the name. The word judged is the one
    // the caller wrote, whatever the parsed value's `String` arm holds:
    // `123`, `true`, `null` and `3.14` each parse as another YAML shape, and a
    // shape that is no scalar at all reads back as no preset either.
    if matches!(written_key.as_str(), "output.theme" | "output.theme.name")
        && let Some(accepted) = crate::cli::unknown_theme_preset(value)
    {
        return Err(crate::cli::cli_error(
            key,
            "invalid_value",
            format!("`{value}` is not a theme preset; accepted names: {accepted}"),
            serde_json::json!({
                "path": cfgd_core::to_posix_string(config_path),
                "value": value,
                "accepted": cfgd_core::output::Theme::PRESET_NAMES,
            }),
        ));
    }

    let mutate_result = mutate_config_yaml(config_path, |raw| {
        let spec = spec_mapping_mut(raw, config_path)?;
        if nested.is_some() {
            let flat = serde_yaml::Value::String(
                key.split_once('.')
                    .map_or(key, |(head, _)| head)
                    .to_string(),
            );
            if let Some(prior) = spec.remove(&flat) {
                previous = serde_json::to_value(&prior).unwrap_or(serde_json::Value::Null);
            }
        }
        let (parent, leaf_key) = walk_spec_path_mut(spec, &written_key)?;
        let yaml_key = serde_yaml::Value::String(leaf_key);
        if let Some(prior) = parent.get(&yaml_key) {
            previous = serde_json::to_value(prior).unwrap_or(serde_json::Value::Null);
        }
        parent.insert(yaml_key, parsed_value.clone());
        Ok(())
    });

    if let Err(e) = mutate_result {
        let e = as_asked(e, key, &written_key);
        let kind = classify_config_error(&e);
        let msg = format!("{}", e);
        let hints = writability_hint(kind, config_path);
        return Err(crate::cli::cli_error_ctx_with_hints(
            e,
            key,
            kind,
            msg,
            serde_json::json!({ "path": cfgd_core::to_posix_string(config_path) }),
            hints,
        ));
    }

    let value_json: serde_json::Value =
        serde_json::to_value(&parsed_value).unwrap_or(serde_json::Value::Null);

    printer.emit(
        Doc::new()
            .status(Role::Ok, format!("Set {} = {}", written_key, value))
            .with_data(serde_json::json!({
                "key": written_key,
                "value": value_json,
                "previousValue": previous,
            })),
    );

    Ok(())
}

pub fn cmd_config_unset(cli: &Cli, printer: &Printer, key: &str) -> anyhow::Result<()> {
    // The `spec.` prefix the docs and `cfgd explain` print is folded away
    // first, so every later read of the key — the walk, the confirmation, the
    // `-o json` payload and the error — names one field.
    let key = spec_relative_key(key);
    let config_path = &cli.config;
    if !config_path.exists() {
        return Err(no_config_error(printer, config_path));
    }

    let mut previous: serde_json::Value = serde_json::Value::Null;

    let nested = nested_output_key(key);
    let written_key = nested.clone().unwrap_or_else(|| key.to_string());
    let mutate_result = mutate_config_yaml(config_path, |raw| {
        let spec = spec_mapping_mut(raw, config_path)?;
        // Unsetting a presentation knob clears both spellings: one left
        // standing is a value the reader believes they removed.
        let mut removed_flat = false;
        if nested.is_some() {
            let flat = serde_yaml::Value::String(
                key.split_once('.')
                    .map_or(key, |(head, _)| head)
                    .to_string(),
            );
            if let Some(prior) = spec.remove(&flat) {
                previous = serde_json::to_value(&prior).unwrap_or(serde_json::Value::Null);
                removed_flat = true;
            }
        }
        let (parent, leaf_key) = walk_spec_path_mut(spec, &written_key)?;
        let yaml_key = serde_yaml::Value::String(leaf_key.clone());
        match parent.remove(&yaml_key) {
            Some(prior) => {
                previous = serde_json::to_value(&prior).unwrap_or(serde_json::Value::Null);
                Ok(())
            }
            None if removed_flat => Ok(()),
            None => Err(anyhow::Error::new(cfgd_core::errors::CfgdError::Config(
                cfgd_core::errors::ConfigError::KeyNotFound {
                    key: key.to_string(),
                },
            ))),
        }
    });

    let written = match mutate_result {
        Ok(written) => written,
        Err(e) => {
            let e = as_asked(e, key, &written_key);
            let kind = classify_config_error(&e);
            let msg = format!("{}", e);
            let hints = writability_hint(kind, config_path);
            return Err(crate::cli::cli_error_ctx_with_hints(
                e,
                key,
                kind,
                msg,
                serde_json::json!({ "path": cfgd_core::to_posix_string(config_path) }),
                hints,
            ));
        }
    };

    // A key with a default inside a section the document keeps is written
    // back at that default, so the file still names it: the row and the
    // payload report the reset, and `removed` holds only for a key the file
    // no longer carries.
    let document_key = format!("spec.{written_key}");
    let reset = written.filled.iter().any(|filled| {
        filled == &document_key
            || filled
                .strip_prefix(document_key.as_str())
                .is_some_and(|rest| rest.starts_with('.'))
    });
    let doc = if reset {
        let default =
            typed_value_at(&written.config.spec, &written_key).unwrap_or(serde_yaml::Value::Null);
        let shown = match &default {
            serde_yaml::Value::String(s) => s.clone(),
            other => serde_json::to_string(other).unwrap_or_default(),
        };
        Doc::new()
            .status(Role::Ok, format!("Reset {written_key} to {shown}"))
            .with_data(serde_json::json!({
                "key": written_key,
                "previousValue": previous,
                "removed": false,
                "value": serde_json::to_value(&default).unwrap_or(serde_json::Value::Null),
            }))
    } else {
        Doc::new()
            .status(Role::Ok, format!("Unset {}", written_key))
            .with_data(serde_json::json!({
                "key": written_key,
                "previousValue": previous,
                "removed": true,
            }))
    };
    printer.emit(doc);

    Ok(())
}

/// Classify a `config get`/`set`/`unset` failure into a stable error_kind for
/// the emit-then-bail Doc payload. Falls back to `invalid_value` for shapes
/// that don't match the known buckets (parse-fail / not-found / no-spec).
///
/// The typed errors are read first, and they are what tells a key that is not
/// there from a document whose shape contradicts the schema: the two failures
/// the walkers used to word alike, which left the human channel saying one
/// thing and `-o json` the other.
fn classify_config_error(e: &anyhow::Error) -> &'static str {
    // A read-only config dir is a distinct, scriptable failure: the pre-flight in
    // mutate_config_yaml surfaces a typed TargetNotWritable.
    match e.downcast_ref::<cfgd_core::errors::CfgdError>() {
        Some(cfgd_core::errors::CfgdError::File(
            cfgd_core::errors::FileError::TargetNotWritable { .. },
        )) => return "target_not_writable",
        Some(cfgd_core::errors::CfgdError::Config(
            cfgd_core::errors::ConfigError::KeyNotFound { .. },
        )) => return "key_not_found",
        _ => {}
    }
    if e.downcast_ref::<ShapeBlocked>().is_some() {
        return "parse_failed";
    }
    let msg = e.to_string();
    if msg.contains("not found") {
        "key_not_found"
    } else if msg.contains("would become invalid") {
        "parse_failed"
    } else {
        "invalid_value"
    }
}

/// Remediation hint for a `target_not_writable` mutate failure naming the config
/// directory, or none for other failure kinds. Centralized so `config set` and
/// `config unset` attach the identical chmod guidance.
///
/// Unconditional: the write refused, and the way out of a refusal is not a
/// tutorial `spec.output.usageHints` gets to suppress.
pub(in crate::cli) fn writability_hint(
    kind: &str,
    config_path: &Path,
) -> Vec<cfgd_core::output::HintCommands> {
    if kind == "target_not_writable"
        && let Some(parent) = config_path.parent()
    {
        return vec![cfgd_core::output::HintCommands::unconditional(format!(
            "check directory permissions: chmod u+w {}",
            cfgd_core::to_posix_string(parent)
        ))];
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cfgd_core::output::OutputFormat;
    use cfgd_core::test_helpers::test_printer;

    fn test_cli_for(config_path: std::path::PathBuf) -> Cli {
        Cli {
            config: config_path,
            config_explicit: false,
            profile: None,
            verbose: 0,
            quiet: true,
            no_color: true,
            color: crate::cli::ColorWhen::Auto,
            output: OutputFormatArg(cfgd_core::output::OutputFormat::Table),
            list_envelope: false,
            hints: false,
            no_hints: false,
            theme: None,
            mask_env_values: None,
            migration_policy: None,
            update_policy: None,
            jsonpath: None,
            yes: false,
            state_dir: None,
            config_dir: None,
            cache_dir: None,
            runtime_dir: None,
            scope_arg: crate::cli::ScopeArg::User,
            command: None,
        }
    }

    /// Assert a config-command error follows the no-config contract: it
    /// downcasts to the typed `ConfigError::NotFound` (so `exit_code_for_error`
    /// maps it to `NoConfig` = 3) and its message names the missing path.
    fn assert_no_config_error(err: &anyhow::Error, expected_path: &std::path::Path) {
        let cfgd_err = err
            .downcast_ref::<cfgd_core::errors::CfgdError>()
            .expect("typed CfgdError");
        assert!(
            matches!(
                cfgd_err,
                cfgd_core::errors::CfgdError::Config(
                    cfgd_core::errors::ConfigError::NotFound { .. }
                )
            ),
            "expected ConfigError::NotFound, got: {cfgd_err}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("config file not found"),
            "message should match plan's wording: {msg}"
        );
        let name = expected_path.file_name().unwrap().to_string_lossy();
        assert!(msg.contains(&*name), "message should name the path: {msg}");
    }

    /// Minimal valid `Config` kind YAML that load_config will accept.
    const SAMPLE_CONFIG: &str = r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  profile: work
  output:
    theme:
      name: monokai
"#;

    fn write_sample_config(dir: &std::path::Path) -> std::path::PathBuf {
        let path = dir.join("cfgd.yaml");
        std::fs::write(&path, SAMPLE_CONFIG).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn cmd_config_set_readonly_dir_yields_target_not_writable_with_path_and_hint_as_non_root() {
        use std::os::unix::fs::PermissionsExt;

        // Root bypasses mode bits; the probe (correctly) reports a 0o500 dir
        // writable to uid 0, so this case cannot be exercised under root.
        if cfgd_core::is_root() {
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let config_path = write_sample_config(dir.path());
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();

        let cli = test_cli_for(config_path.clone());
        let printer = test_printer();
        let err = cmd_config_set(&cli, &printer, "theme.name", "nord")
            .expect_err("read-only config dir must reject the mutation");

        let cfgd_err = err
            .downcast_ref::<cfgd_core::errors::CfgdError>()
            .expect("typed CfgdError in chain");
        assert!(matches!(
            cfgd_err,
            cfgd_core::errors::CfgdError::File(
                cfgd_core::errors::FileError::TargetNotWritable { .. }
            )
        ));
        let meta = err
            .downcast_ref::<crate::cli::CliErrorMeta>()
            .expect("CliErrorMeta carrier");
        assert_eq!(meta.error_kind, "target_not_writable");
        assert!(
            meta.hints.iter().any(|h| h.text.contains("chmod u+w")),
            "expected a chmod remediation hint, got: {:?}",
            meta.hints
        );

        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    // --- parse_yaml_value ---

    #[test]
    fn parse_yaml_value_dispatches_each_type() {
        assert_eq!(parse_yaml_value("true"), serde_yaml::Value::Bool(true));
        assert_eq!(parse_yaml_value("false"), serde_yaml::Value::Bool(false));
        assert_eq!(parse_yaml_value("null"), serde_yaml::Value::Null);
        assert_eq!(parse_yaml_value("~"), serde_yaml::Value::Null);
        assert_eq!(
            parse_yaml_value("42"),
            serde_yaml::Value::Number(42i64.into())
        );
        assert!(matches!(
            parse_yaml_value("3.14"),
            serde_yaml::Value::Number(_)
        ));
        assert_eq!(
            parse_yaml_value("hello"),
            serde_yaml::Value::String("hello".into())
        );
        // Empty string falls through to String, not anything else.
        assert_eq!(parse_yaml_value(""), serde_yaml::Value::String("".into()));
    }

    // --- walk_yaml_path ---

    #[test]
    fn walk_yaml_path_dot_returns_root() {
        let yaml: serde_yaml::Value = serde_yaml::from_str("a: 1\n").unwrap();
        let leaf = walk_yaml_path(&yaml, ".").unwrap();
        // Root is the whole mapping
        assert!(leaf.is_mapping());
    }

    #[test]
    fn walk_yaml_path_nested_segments_resolve_leaf() {
        let yaml: serde_yaml::Value = serde_yaml::from_str("theme:\n  name: monokai\n").unwrap();
        let leaf = walk_yaml_path(&yaml, "theme.name").unwrap();
        assert_eq!(leaf, &serde_yaml::Value::String("monokai".into()));
    }

    // 'a' exists and holds a scalar the schema declares no fields under, so
    // 'a.b' is a key that can never exist — the walk names the path it could
    // not reach.
    #[test]
    fn walk_yaml_path_missing_key_errs_with_partial_path() {
        let yaml: serde_yaml::Value = serde_yaml::from_str("a: 1\n").unwrap();
        let err = walk_yaml_path(&yaml, "a.b.c").unwrap_err();
        assert_eq!(err.to_string(), "config error: key 'a.b' not found");
    }

    // The other half: `daemon` is a mapping in the Config schema, so a scalar
    // there is a document whose shape contradicts it, and the refusal says
    // which shape it found.
    #[test]
    fn walk_yaml_path_at_a_declared_mapping_names_the_shape_it_found() {
        let yaml: serde_yaml::Value = serde_yaml::from_str("daemon: yes\n").unwrap();
        let err = walk_yaml_path(&yaml, "daemon.reconcile").unwrap_err();
        assert_eq!(
            err.to_string(),
            "'daemon' holds a scalar where a mapping belongs"
        );
    }

    // The document itself is no key, so its refusal names it in words; no
    // path is quoted.
    #[test]
    fn a_config_document_that_is_not_a_mapping_is_refused_in_its_own_words() {
        let mut root: serde_yaml::Value = serde_yaml::from_str("just a string\n").unwrap();
        let err = spec_mapping_mut(&mut root, Path::new("cfgd.yaml")).unwrap_err();
        assert_eq!(
            err.to_string(),
            "the config document holds a scalar where a mapping belongs"
        );
        assert!(
            err.chain()
                .any(|e| e.downcast_ref::<ShapeBlocked>().is_some()),
            "the refusal carries the typed shape block: {err:?}"
        );
    }

    /// Every `spec`-relative path the `Config` schema names that a key path
    /// can address: each top-level field, and then each field under one the
    /// schema declares a mapping at, to any depth. A sequence's element fields
    /// are left out because no key path names an element.
    fn addressable_config_paths() -> Vec<Vec<String>> {
        addressable_config_fields()
            .into_iter()
            .map(|(path, _)| path)
            .collect()
    }

    /// [`addressable_config_paths`] with the schema node each path names.
    fn addressable_config_fields() -> Vec<(Vec<String>, cfgd_core::schema::FieldNode)> {
        fn walk(
            fields: &[cfgd_core::schema::FieldNode],
            at: &[String],
            out: &mut Vec<(Vec<String>, cfgd_core::schema::FieldNode)>,
        ) {
            for field in fields {
                if field.is_variant {
                    continue;
                }
                let mut path = at.to_vec();
                path.push(field.name.clone());
                out.push((path.clone(), field.clone()));
                if field.type_desc == "object" {
                    walk(&field.children, &path, out);
                }
            }
        }
        let schema = crate::cli::explain::find_schema("Config").expect("the Config schema");
        let mut out = Vec::new();
        walk(&schema.fields, &[], &mut out);
        out
    }

    /// Every scalar leaf [`addressable_config_fields`] reads, as the dotted
    /// key `config set` takes and a value the parser accepts for the leaf's
    /// declared type: `true` for a boolean, the first accepted word of an
    /// enum, and for a free string the value the typed config gives the leaf
    /// once its section is present, or a plain word where it gives none.
    fn settable_config_leaves() -> Vec<(String, String)> {
        let path = std::path::Path::new("cfgd.yaml");
        addressable_config_fields()
            .into_iter()
            .filter(|(_, node)| node.type_desc != "object" && !node.type_desc.starts_with("[]"))
            .map(|(segments, node)| {
                let key = segments.join(".");
                let value = if node.type_desc == "boolean" {
                    "true".to_string()
                } else if let Some(word) = node.enum_values.first() {
                    word.clone()
                } else {
                    let section = spec_holding(
                        &segments[..segments.len() - 1],
                        serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
                    );
                    let doc = format!(
                        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n{}",
                        serde_yaml::to_string(&section)
                            .unwrap()
                            .lines()
                            .map(|l| format!("  {l}\n"))
                            .collect::<String>()
                    );
                    cfgd_core::config::parse_config(&doc, path)
                        .ok()
                        .and_then(|cfg| serde_yaml::to_value(cfg.spec).ok())
                        .and_then(|spec| walk_yaml_path(&spec, &key).ok().cloned())
                        .and_then(|v| v.as_str().map(str::to_string))
                        .unwrap_or_else(|| "probe".to_string())
                };
                (key, value)
            })
            .collect()
    }

    /// A fresh `cfgd init` scaffold in `dir`, returning the `cfgd.yaml` path.
    fn scaffolded(dir: &std::path::Path) -> std::path::PathBuf {
        crate::cli::init::cmd_init::scaffold(dir, Some("t"), None, &test_printer()).unwrap();
        dir.join("cfgd.yaml")
    }

    /// The same scaffold as a `cfgd.toml`, the document form `--config` also
    /// accepts, returning its path.
    fn toml_scaffolded(dir: &std::path::Path) -> std::path::PathBuf {
        let yaml = scaffolded(dir);
        let tree: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(&yaml).unwrap()).unwrap();
        std::fs::remove_file(&yaml).unwrap();
        let path = dir.join("cfgd.toml");
        std::fs::write(&path, toml::to_string(&tree).unwrap()).unwrap();
        path
    }

    /// Why a written document is not aligned: the keys the load-time gate
    /// would still ask about, and every scalar of the section holding `key`
    /// that the typed value carries and the bytes do not declare.
    fn misalignment(path: &std::path::Path, key: &str) -> Option<String> {
        let bytes = std::fs::read_to_string(path).unwrap();
        let parsed = match cfgd_core::config::parse_config(&bytes, path) {
            Ok(parsed) => parsed,
            Err(e) => return Some(format!("the written document does not parse: {e}")),
        };
        let pending = crate::cli::config_schema::pending_alignment(&parsed, &bytes, path).keys;
        let declared = crate::cli::source::config_tree(&bytes, path).unwrap();
        let typed = serde_yaml::to_value(&parsed.spec).unwrap();
        let section = key.rsplit_once('.').map_or(".", |(parent, _)| parent);
        let missing: Vec<String> = walk_yaml_path(&typed, section)
            .ok()
            .and_then(serde_yaml::Value::as_mapping)
            .into_iter()
            .flatten()
            .filter(|(_, v)| {
                matches!(
                    v,
                    serde_yaml::Value::Bool(_)
                        | serde_yaml::Value::Number(_)
                        | serde_yaml::Value::String(_)
                )
            })
            .filter_map(|(k, _)| {
                let sibling = match section {
                    "." => format!("spec.{}", k.as_str()?),
                    parent => format!("spec.{parent}.{}", k.as_str()?),
                };
                walk_yaml_path(&declared, &sibling)
                    .is_err()
                    .then_some(sibling)
            })
            .collect();
        (!pending.is_empty() || !missing.is_empty())
            .then(|| format!("pending {pending:?}, section siblings not declared {missing:?}"))
    }

    /// The scalar leaves the Config schema names today; a walk finding fewer
    /// has stopped reading the schema.
    const SETTABLE_LEAF_FLOOR: usize = 63;

    // The population is every scalar leaf the schema names, so a field added
    // to any section later is set here by being declared. A `config set` that
    // brings a section into existence (`daemon.reconcile.autoApply` on a
    // fresh scaffold, which declares no `daemon`) must leave that section
    // declaring every sibling the build reads, or the next command's
    // migration gate asks about fields the reader never touched.
    #[test]
    fn every_config_document_write_leaves_it_aligned() {
        let leaves = settable_config_leaves();
        assert!(
            leaves.len() >= SETTABLE_LEAF_FLOOR,
            "{} settable leaves, fewer than the {SETTABLE_LEAF_FLOOR} the schema names today",
            leaves.len()
        );
        assert!(
            leaves
                .iter()
                .any(|(key, _)| key == "daemon.reconcile.autoApply"),
            "the leaf the defect was found on is in the population"
        );
        let printer = test_printer();
        let mut offenders = Vec::new();
        for toml in [false, true] {
            for (key, value) in &leaves {
                let dir = tempfile::tempdir().unwrap();
                let path = if toml {
                    toml_scaffolded(dir.path())
                } else {
                    scaffolded(dir.path())
                };
                let cli = test_cli_for(path.clone());
                let form = if toml { "cfgd.toml" } else { "cfgd.yaml" };
                let case = format!("config set {key} {value} on {form}");
                if let Err(e) = cmd_config_set(&cli, &printer, key, value) {
                    offenders.push(format!("{case}: refused: {e}"));
                    continue;
                }
                if toml {
                    let written = std::fs::read_to_string(&path).unwrap();
                    if let Err(e) = written.parse::<toml::Table>() {
                        offenders.push(format!("{case}: the document is no longer TOML: {e}"));
                        continue;
                    }
                }
                if let Some(why) = misalignment(&path, key) {
                    offenders.push(format!("{case}: {why}"));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "a config set left the document partial:\n{}",
            offenders.join("\n")
        );
    }

    // The unset half, on a document declaring every leaf: removing a key
    // with a default re-declares it at that default, and removing the last
    // key of a section leaves either the section complete or no section at
    // all, both of which the gate reads as aligned. The payload says which
    // of the two happened: `removed` only for a key the file no longer
    // carries, and the value written back for a key reset to its default.
    #[test]
    fn every_config_document_unset_leaves_it_aligned() {
        let leaves = settable_config_leaves();
        let printer = test_printer();
        let full = tempfile::tempdir().unwrap();
        let full_path = scaffolded(full.path());
        let full_cli = test_cli_for(full_path.clone());
        for (key, value) in &leaves {
            cmd_config_set(&full_cli, &printer, key, value)
                .unwrap_or_else(|e| panic!("config set {key} {value}: {e}"));
        }
        let full_doc = std::fs::read_to_string(&full_path).unwrap();

        let mut offenders = Vec::new();
        let mut payloads = std::collections::BTreeMap::new();
        for (key, _) in &leaves {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("cfgd.yaml");
            std::fs::write(&path, &full_doc).unwrap();
            let cli = test_cli_for(path.clone());
            let (json_printer, cap) = Printer::for_test_doc_with_format(OutputFormat::Json);
            if let Err(e) = cmd_config_unset(&cli, &json_printer, key) {
                offenders.push(format!("config unset {key}: refused: {e}"));
                continue;
            }
            drop(json_printer);
            if let Some(why) = misalignment(&path, key) {
                offenders.push(format!("config unset {key}: {why}"));
            }
            let payload = cap.json().expect("doc captured json");
            let written: serde_yaml::Value =
                serde_yaml::from_str(&cfgd_core::test_helpers::walked_file_body(&path)).unwrap();
            let on_file = walk_yaml_path(&written, &format!("spec.{key}")).ok();
            match on_file {
                None if payload["removed"] != true => offenders.push(format!(
                    "config unset {key}: the file no longer carries it, the payload says {payload}"
                )),
                Some(value)
                    if payload["removed"] != false
                        || payload["value"] != serde_json::to_value(value).unwrap() =>
                {
                    offenders.push(format!(
                        "config unset {key}: the file holds {value:?}, the payload says {payload}"
                    ))
                }
                _ => {}
            }
            payloads.insert(key.clone(), payload);
        }
        assert!(
            offenders.is_empty(),
            "a config unset left the document partial or misreported it:\n{}",
            offenders.join("\n")
        );

        let reset = &payloads["daemon.reconcile.autoApply"];
        assert_eq!(
            (&reset["removed"], &reset["value"]),
            (&serde_json::json!(false), &serde_json::json!(false)),
            "a field with a default is reported reset to that default: {reset}"
        );
        let removed = &payloads["profile"];
        assert_eq!(
            (&removed["removed"], removed.get("value")),
            (&serde_json::json!(true), None),
            "an optional field is reported removed: {removed}"
        );
    }

    /// A `spec` document holding `leaf` at `path` and nothing else.
    fn spec_holding(path: &[String], leaf: serde_yaml::Value) -> serde_yaml::Value {
        path.iter().rev().fold(leaf, |value, segment| {
            let mut map = serde_yaml::Mapping::new();
            map.insert(serde_yaml::Value::String(segment.clone()), value);
            serde_yaml::Value::Mapping(map)
        })
    }

    // The population walk behind `descent_blocked`: for every field the Config
    // schema names, plant a value of the wrong shape under it and ask for a key
    // beneath it, then check the refusal against what the schema declares
    // there. A free-form map (`spec.aliases`) is a mapping that names no child
    // field, and a list is a shape the key walker cannot address, which the
    // document did not get wrong; both read the same as their neighbours under
    // a child count.
    #[test]
    fn every_config_spec_field_refuses_a_wrong_shape_by_its_declared_shape() {
        use crate::cli::explain::DeclaredShape;

        let paths = addressable_config_paths();
        assert!(
            paths.len() >= 60,
            "the Config schema names far more addressable fields than this: {}",
            paths.len()
        );
        for expected in ["aliases", "sources", "origin", "daemon", "fileStrategy"] {
            assert!(
                paths.iter().any(|p| p == &[expected.to_string()]),
                "{expected} is in the population"
            );
        }

        // The loop below reads its expected refusal from `config_field_shape`,
        // the same function `descent_blocked` asks, so it pins the mapping
        // itself. These rows say what the schema declares, so an oracle
        // that answers a child count again fails here. One row per `type_desc`
        // spelling the reflection holds, because the spelling is what the
        // oracle branches on: a demoted `[]string` or `boolean` arm reads as a
        // scalar, the loop below agrees with it, and only a row named at that
        // spelling can see it.
        for (path, expected) in [
            // object, no children of its own
            (&["aliases"][..], DeclaredShape::Mapping),
            // object, with children
            (&["daemon"][..], DeclaredShape::Mapping),
            // []object
            (&["sources"][..], DeclaredShape::Sequence),
            (&["origin"][..], DeclaredShape::Sequence),
            // []string
            (
                &["compliance", "scope", "watchPaths"][..],
                DeclaredShape::Sequence,
            ),
            // string
            (&["fileStrategy"][..], DeclaredShape::Leaf),
            // boolean
            (&["daemon", "enabled"][..], DeclaredShape::Leaf),
            // the schema names nothing here
            (&["nope"][..], DeclaredShape::Unknown),
        ] {
            assert_eq!(
                crate::cli::explain::config_field_shape(path),
                expected,
                "the declared shape at {path:?}"
            );
        }

        let scalar = serde_yaml::Value::String("planted".into());
        let sequence = serde_yaml::Value::Sequence(vec![scalar.clone()]);
        for path in &paths {
            let segments: Vec<&str> = path.iter().map(String::as_str).collect();
            let declared = crate::cli::explain::config_field_shape(&segments);
            let key = format!("{}.probe", path.join("."));
            // A scalar-or-mapping union's scalar arm IS its mapping with one
            // field set, so it is settled before the shape question is asked
            // and `probe` is simply another field of the arm.
            let union_arm = scalar_union_field(&segments).is_some();

            for (planted, expected) in [
                (
                    scalar.clone(),
                    match declared {
                        _ if union_arm => "key_not_found",
                        DeclaredShape::Mapping | DeclaredShape::Sequence => "parse_failed",
                        DeclaredShape::Leaf | DeclaredShape::Unknown => "key_not_found",
                    },
                ),
                (
                    sequence.clone(),
                    match declared {
                        DeclaredShape::Sequence => "key_not_found",
                        _ => "parse_failed",
                    },
                ),
            ] {
                let spec = spec_holding(path, planted.clone());
                let err = walk_yaml_path(&spec, &key)
                    .err()
                    .unwrap_or_else(|| panic!("{key} resolves nothing on {planted:?}"));
                assert_eq!(
                    classify_config_error(&err),
                    expected,
                    "read walk on {key} over {planted:?} (declared {declared:?}): {err}"
                );

                // The setter promotes a union's scalar arm, so only the read
                // walk answers there.
                if union_arm && planted.is_string() {
                    continue;
                }
                let mut spec = spec_holding(path, planted.clone());
                let err = walk_yaml_path_mut(&mut spec, &key)
                    .err()
                    .unwrap_or_else(|| panic!("{key} is writable on {planted:?}"));
                assert_eq!(
                    classify_config_error(&err),
                    expected,
                    "write walk on {key} over {planted:?} (declared {declared:?}): {err}"
                );
            }
        }
    }

    // The schema names no `a`, so a value there is a leaf as far as the key
    // walker can tell, and a sequence standing at one is a shape the document
    // got wrong. A list the walker declines to index into is a different case,
    // which the document did not get wrong.
    #[test]
    fn walk_yaml_path_blocked_by_a_sequence_names_it() {
        let yaml: serde_yaml::Value = serde_yaml::from_str("a:\n  - 1\n").unwrap();
        let err = walk_yaml_path(&yaml, "a.b").unwrap_err();
        assert_eq!(
            err.to_string(),
            "'a' holds a sequence where a mapping belongs"
        );
    }

    #[test]
    fn walk_yaml_path_empty_segment_errs() {
        let yaml: serde_yaml::Value = serde_yaml::from_str("a:\n  b: 1\n").unwrap();
        let err = walk_yaml_path(&yaml, "a..b").unwrap_err();
        assert!(
            err.to_string().contains("empty segment"),
            "expected 'empty segment' error, got: {err}"
        );
    }

    #[test]
    fn walk_yaml_path_unknown_top_level_key_errs() {
        let yaml: serde_yaml::Value = serde_yaml::from_str("a: 1\n").unwrap();
        let err = walk_yaml_path(&yaml, "nope").unwrap_err();
        assert!(
            err.to_string().contains("'nope' not found"),
            "expected key-not-found error, got: {err}"
        );
    }

    // --- walk_yaml_path_mut ---

    #[test]
    fn walk_yaml_path_mut_creates_intermediate_maps() {
        let mut yaml: serde_yaml::Value = serde_yaml::from_str("existing: 1\n").unwrap();
        let (parent, leaf) = walk_yaml_path_mut(&mut yaml, "a.b.c").unwrap();
        assert_eq!(leaf, "c");
        // Insert and verify the chain was materialized
        parent.insert(
            serde_yaml::Value::String("c".into()),
            serde_yaml::Value::Bool(true),
        );
        let leaf_val = walk_yaml_path(&yaml, "a.b.c").unwrap();
        assert_eq!(leaf_val, &serde_yaml::Value::Bool(true));
    }

    #[test]
    fn walk_yaml_path_mut_empty_segment_errs() {
        let mut yaml: serde_yaml::Value = serde_yaml::from_str("a: 1\n").unwrap();
        let err = walk_yaml_path_mut(&mut yaml, "a..b").unwrap_err();
        assert!(
            err.to_string().contains("empty segment"),
            "expected 'empty segment' error, got: {err}"
        );
    }

    // --- cmd_config_show ---

    #[test]
    fn cmd_config_show_missing_file_bails_with_no_config_msg() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.yaml");
        let cli = test_cli_for(path.clone());
        let printer = test_printer();

        let err = cmd_config_show(&cli, &printer).unwrap_err();
        assert_no_config_error(&err, &path);
    }

    #[test]
    fn cmd_config_show_table_renders_header_and_profile() {
        let dir = tempfile::tempdir().unwrap();
        let cli = test_cli_for(write_sample_config(dir.path()));
        let (printer, cap) = Printer::for_test_doc();

        cmd_config_show(&cli, &printer).unwrap();
        printer.flush();
        drop(printer);

        let output = cap.human();
        assert!(
            output.contains("Configuration"),
            "should print 'Configuration' header, got: {output}"
        );
        assert!(
            output.contains("work"),
            "should print profile value 'work', got: {output}"
        );
    }

    #[test]
    fn cmd_config_show_json_emits_parseable_object() {
        let dir = tempfile::tempdir().unwrap();
        let cli = test_cli_for(write_sample_config(dir.path()));
        let (printer, buf) = Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

        cmd_config_show(&cli, &printer).unwrap();

        let captured = cfgd_core::test_helpers::captured_text(&buf);
        let parsed: serde_json::Value = serde_json::from_str(captured.trim())
            .unwrap_or_else(|e| panic!("invalid JSON: {e}, got: {captured}"));
        assert_eq!(parsed["apiVersion"], "cfgd.io/v1alpha1");
        assert_eq!(parsed["kind"], "Config");
        assert_eq!(parsed["spec"]["profile"], "work");
    }

    // --- cmd_config_get ---

    #[test]
    fn cmd_config_get_missing_file_bails_with_no_config_msg() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.yaml");
        let cli = test_cli_for(path.clone());
        let printer = test_printer();

        let err = cmd_config_get(&cli, &printer, "profile").unwrap_err();
        assert_no_config_error(&err, &path);
    }

    /// `theme.name` and `output.theme.name` name one field: `get` answers
    /// either spelling from the nested block a migrated document carries.
    #[test]
    fn cmd_config_get_answers_a_legacy_key_from_the_nested_block() {
        let dir = tempfile::tempdir().unwrap();
        let cli = test_cli_for(write_sample_config(dir.path()));
        let (printer, cap) = Printer::for_test_doc();

        cmd_config_get(&cli, &printer, "theme.name").unwrap();
        drop(printer);

        assert_eq!(cap.human().trim(), "monokai");
    }

    /// A document still carrying the flat key is answered from it, so `get`
    /// never reports a key the file visibly holds as missing.
    #[test]
    fn cmd_config_get_falls_back_to_the_flat_key_a_document_still_spells() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfgd.yaml");
        std::fs::write(
            &path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: work\n  theme:\n    name: nord\n",
        )
        .unwrap();
        let cli = test_cli_for(path);
        let (printer, cap) = Printer::for_test_doc();

        cmd_config_get(&cli, &printer, "output.theme.name").unwrap();
        drop(printer);

        assert_eq!(cap.human().trim(), "nord");
    }

    /// Whichever spelling the caller reached for, the write lands under
    /// `spec.output` and the flat key it replaced is dropped with it.
    #[test]
    fn cmd_config_set_writes_the_nested_key_and_drops_the_flat_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfgd.yaml");
        std::fs::write(
            &path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: work\n  theme:\n    name: nord\n",
        )
        .unwrap();
        let cli = test_cli_for(path.clone());
        let printer = test_printer();

        cmd_config_set(&cli, &printer, "theme.name", "dracula").unwrap();

        let written: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let spec = written.get("spec").expect("spec survives the rewrite");
        assert!(
            spec.get("theme").is_none(),
            "the flat key must be gone, got: {spec:?}"
        );
        assert_eq!(
            spec.get("output")
                .and_then(|o| o.get("theme"))
                .and_then(|t| t.get("name"))
                .and_then(serde_yaml::Value::as_str),
            Some("dracula")
        );
    }

    /// Unsetting clears both spellings: one left standing is a value the
    /// reader believes they removed.
    #[test]
    fn cmd_config_unset_clears_both_spellings_of_a_presentation_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfgd.yaml");
        std::fs::write(
            &path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: work\n  theme: nord\n  output:\n    theme:\n      name: dracula\n",
        )
        .unwrap();
        let cli = test_cli_for(path.clone());
        let printer = test_printer();

        cmd_config_unset(&cli, &printer, "theme").unwrap();

        let written: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let spec = written.get("spec").expect("spec survives the rewrite");
        assert!(spec.get("theme").is_none(), "flat key left standing");
        assert!(
            spec.get("output").is_none_or(|o| o.get("theme").is_none()),
            "nested key left standing: {spec:?}"
        );
    }

    #[test]
    fn cmd_config_get_scalar_prints_value_only() {
        let dir = tempfile::tempdir().unwrap();
        let cli = test_cli_for(write_sample_config(dir.path()));
        let (printer, cap) = Printer::for_test_doc();

        cmd_config_get(&cli, &printer, "profile").unwrap();
        drop(printer);

        let captured = cap.human();
        assert_eq!(
            captured.trim(),
            "work",
            "scalar get should print bare value, got: {captured:?}"
        );
    }

    #[test]
    fn cmd_config_get_nested_key_prints_value() {
        let dir = tempfile::tempdir().unwrap();
        let cli = test_cli_for(write_sample_config(dir.path()));
        let (printer, cap) = Printer::for_test_doc();

        cmd_config_get(&cli, &printer, "theme.name").unwrap();
        drop(printer);

        let captured = cap.human();
        assert_eq!(captured.trim(), "monokai");
    }

    #[test]
    fn cmd_config_get_unknown_key_errs() {
        let dir = tempfile::tempdir().unwrap();
        let cli = test_cli_for(write_sample_config(dir.path()));
        let printer = test_printer();

        let err = cmd_config_get(&cli, &printer, "missing").unwrap_err();
        assert!(
            err.to_string().contains("'missing' not found"),
            "expected key-not-found error, got: {err}"
        );

        // A missing config key is the same scriptable "named thing not there"
        // condition as a missing profile/module — it must carry the typed
        // ConfigError::KeyNotFound so the exit code resolves to NotFound (6),
        // not the generic ExitCode::Error (1) a bare anyhow error collapses to.
        let cfgd_err = err
            .downcast_ref::<cfgd_core::errors::CfgdError>()
            .expect("a missing config key must carry the typed ConfigError::KeyNotFound");
        assert!(
            matches!(
                cfgd_err,
                cfgd_core::errors::CfgdError::Config(
                    cfgd_core::errors::ConfigError::KeyNotFound { .. }
                )
            ),
            "expected ConfigError::KeyNotFound, got: {cfgd_err}"
        );
        assert_eq!(
            cfgd_core::exit::exit_code_for_error(cfgd_err),
            cfgd_core::exit::ExitCode::NotFound
        );
    }

    #[test]
    fn cmd_config_get_no_spec_section_errs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nospec.yaml");
        std::fs::write(&path, "apiVersion: cfgd.io/v1alpha1\nkind: Config\n").unwrap();
        let cli = test_cli_for(path);
        let printer = test_printer();

        let err = cmd_config_get(&cli, &printer, "profile").unwrap_err();
        assert!(
            err.to_string().contains("no 'spec' section"),
            "expected 'no spec section' error, got: {err}"
        );
    }

    #[test]
    fn cmd_config_get_json_emits_parseable_value() {
        let dir = tempfile::tempdir().unwrap();
        let cli = test_cli_for(write_sample_config(dir.path()));
        let (printer, cap) = Printer::for_test_doc_with_format(OutputFormat::Json);

        cmd_config_get(&cli, &printer, "theme").unwrap();
        drop(printer);

        let parsed = cap.json().expect("doc captured json");
        assert_eq!(parsed["key"], "theme");
        assert_eq!(parsed["value"]["name"], "monokai");
    }

    /// `--config` accepts a `cfgd.toml`, so `config get` reads one in its own
    /// format: every shape a YAML document answers with, and a TOML syntax
    /// error refused as the document's own parse failure.
    #[test]
    fn cmd_config_get_reads_a_toml_document_in_its_own_format() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfgd.toml");
        std::fs::write(
            &path,
            "apiVersion = \"cfgd.io/v1alpha1\"\nkind = \"Config\"\n\n[metadata]\nname = \"t\"\n\n\
             [spec]\nprofile = \"work\"\n\n[spec.theme]\nname = \"monokai\"\n",
        )
        .unwrap();
        let cli = test_cli_for(path.clone());

        let (printer, cap) = Printer::for_test_doc();
        cmd_config_get(&cli, &printer, "profile").unwrap();
        drop(printer);
        assert_eq!(cap.human().trim(), "work");

        let (printer, cap) = Printer::for_test_doc();
        cmd_config_get(&cli, &printer, "theme.name").unwrap();
        drop(printer);
        assert_eq!(cap.human().trim(), "monokai");

        let (printer, cap) = Printer::for_test_doc_with_format(OutputFormat::Json);
        cmd_config_get(&cli, &printer, "theme").unwrap();
        drop(printer);
        let parsed = cap.json().expect("doc captured json");
        assert_eq!(parsed["value"]["name"], "monokai");

        std::fs::write(&path, "[spec\nprofile = \"work\"\n").unwrap();
        let err = cmd_config_get(&cli, &test_printer(), "profile").unwrap_err();
        let meta = err
            .downcast_ref::<crate::cli::CliErrorMeta>()
            .expect("CliErrorMeta carrier on parse_failed");
        assert_eq!(meta.error_kind, "parse_failed");
    }

    // TOML has no null, so `~` on a cfgd.toml is refused by name before a
    // byte is written; dropping the key would make the set a silent no-op.
    #[test]
    fn cmd_config_set_null_on_a_toml_document_is_refused_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = toml_scaffolded(dir.path());
        let before = std::fs::read(&path).unwrap();
        let cli = test_cli_for(path.clone());
        let err = cmd_config_set(&cli, &test_printer(), "daemon.enabled", "~").unwrap_err();
        let meta = err
            .downcast_ref::<crate::cli::CliErrorMeta>()
            .expect("CliErrorMeta carrier on parse_failed");
        assert_eq!(meta.error_kind, "parse_failed", "{err}");
        let message = err.to_string();
        assert!(
            message.contains("spec.daemon.enabled") && message.contains("TOML has no null"),
            "the refusal names the key and the reason: {message}"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "a refused write leaves the bytes alone"
        );
    }

    // --- cmd_config_set ---

    #[test]
    fn cmd_config_set_missing_file_bails_with_no_config_msg() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.yaml");
        let cli = test_cli_for(path.clone());
        let printer = test_printer();

        let err = cmd_config_set(&cli, &printer, "profile", "dev").unwrap_err();
        assert_no_config_error(&err, &path);
    }

    #[test]
    fn cmd_config_set_overwrites_scalar_and_persists_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_sample_config(dir.path());
        let cli = test_cli_for(path.clone());
        let printer = test_printer();

        cmd_config_set(&cli, &printer, "profile", "dev").unwrap();

        // Round-trip via the same parser to confirm the write survived validation
        let reloaded: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            reloaded["spec"]["profile"],
            serde_yaml::Value::String("dev".into())
        );
    }

    #[test]
    fn cmd_config_set_special_chars_round_trip_as_string() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_sample_config(dir.path());
        let cli = test_cli_for(path.clone());
        let printer = test_printer();
        let weird = "value with: colon, # hash, and 'quote'";

        cmd_config_set(&cli, &printer, "profile", weird).unwrap();

        let reloaded: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            reloaded["spec"]["profile"],
            serde_yaml::Value::String(weird.into())
        );
    }

    #[test]
    fn cmd_config_set_empty_value_writes_empty_string() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_sample_config(dir.path());
        let cli = test_cli_for(path.clone());
        let printer = test_printer();

        cmd_config_set(&cli, &printer, "profile", "").unwrap();

        let reloaded: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            reloaded["spec"]["profile"],
            serde_yaml::Value::String("".into())
        );
    }

    /// `config set` validates the document it is about to write, so the global
    /// `Patch` rejection reaches the CLI surface too — the file is left
    /// untouched rather than written into a state every apply would fail on.
    #[test]
    fn cmd_config_set_rejects_patch_as_the_global_file_strategy() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_sample_config(dir.path());
        let before = std::fs::read_to_string(&path).unwrap();
        let cli = test_cli_for(path.clone());
        let printer = test_printer();

        let err = cmd_config_set(&cli, &printer, "fileStrategy", "Patch").unwrap_err();
        assert!(
            err.to_string().contains("fileStrategy"),
            "expected the fileStrategy rejection, got: {err}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            before,
            "a rejected set must not write the config"
        );
    }

    #[test]
    fn cmd_config_set_invalid_key_path_errs() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_sample_config(dir.path());
        let cli = test_cli_for(path);
        let printer = test_printer();

        let err = cmd_config_set(&cli, &printer, "a..b", "x").unwrap_err();
        assert!(
            err.to_string().contains("empty segment"),
            "expected 'empty segment' error, got: {err}"
        );
    }

    // --- cmd_config_unset ---

    #[test]
    fn cmd_config_unset_missing_file_bails_with_no_config_msg() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.yaml");
        let cli = test_cli_for(path.clone());
        let printer = test_printer();

        let err = cmd_config_unset(&cli, &printer, "profile").unwrap_err();
        assert_no_config_error(&err, &path);
    }

    #[test]
    fn cmd_config_unset_removes_existing_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_sample_config(dir.path());
        let cli = test_cli_for(path.clone());
        let printer = test_printer();

        cmd_config_unset(&cli, &printer, "profile").unwrap();

        let reloaded: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(
            reloaded["spec"].get("profile").is_none(),
            "profile key should be removed, got: {reloaded:?}"
        );
    }

    #[test]
    fn cmd_config_unset_unknown_key_errs() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_sample_config(dir.path());
        let cli = test_cli_for(path);
        let printer = test_printer();

        let err = cmd_config_unset(&cli, &printer, "missingKey").unwrap_err();
        assert!(
            err.to_string().contains("'missingKey' not found"),
            "expected key-not-found error, got: {err}"
        );

        let cfgd_err = err
            .downcast_ref::<cfgd_core::errors::CfgdError>()
            .expect("a missing config key must carry the typed ConfigError::KeyNotFound");
        assert!(
            matches!(
                cfgd_err,
                cfgd_core::errors::CfgdError::Config(
                    cfgd_core::errors::ConfigError::KeyNotFound { .. }
                )
            ),
            "expected ConfigError::KeyNotFound, got: {cfgd_err}"
        );
        assert_eq!(
            cfgd_core::exit::exit_code_for_error(cfgd_err),
            cfgd_core::exit::ExitCode::NotFound
        );
    }

    // --- coverage: mapping/null rendering and parse-error paths ---

    // (a) Getting a mapping key in human/Table mode renders the YAML block.
    // The `theme` key in SAMPLE_CONFIG is a mapping (`{name: monokai}`).
    // The `other` branch in cmd_config_get serialises it via serde_yaml and
    // strips the leading "---\n" document marker, so the output should be a
    // multi-line YAML block containing `name: monokai`.
    #[test]
    fn cmd_config_get_mapping_in_human_mode_renders_yaml_block() {
        let dir = tempfile::tempdir().unwrap();
        let cli = test_cli_for(write_sample_config(dir.path()));
        let (printer, cap) = Printer::for_test_doc();

        cmd_config_get(&cli, &printer, "theme").unwrap();
        drop(printer);

        let captured = cap.human();
        assert!(
            captured.contains("name: monokai"),
            "expected YAML block with 'name: monokai', got: {captured:?}"
        );
    }

    // (b) A key whose value is explicit YAML null produces empty stdout in human mode.
    #[test]
    fn cmd_config_get_null_value_produces_empty_output_in_human_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfgd.yaml");
        // `profile` is an explicit null here so the Null branch is exercised.
        std::fs::write(
            &path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: null\n",
        )
        .unwrap();
        let cli = test_cli_for(path);
        let (printer, cap) = Printer::for_test_doc();

        cmd_config_get(&cli, &printer, "profile").unwrap();
        drop(printer);

        let captured = cap.human();
        assert!(
            captured.trim().is_empty(),
            "null value should produce empty output, got: {captured:?}"
        );
    }

    // (c) A syntactically broken YAML file fails serde_yaml parsing in
    // cmd_config_get and returns an error with error_kind == "parse_failed".
    #[test]
    fn cmd_config_get_broken_yaml_yields_parse_failed_error_kind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.yaml");
        // This is syntactically invalid YAML: a mapping value followed by a
        // block sequence entry with an empty key, which serde_yaml rejects.
        std::fs::write(&path, "spec:\n  - : :\n").unwrap();
        let cli = test_cli_for(path);
        let printer = test_printer();

        let err = cmd_config_get(&cli, &printer, "profile").unwrap_err();
        let meta = err
            .downcast_ref::<crate::cli::CliErrorMeta>()
            .expect("CliErrorMeta carrier on parse_failed");
        assert_eq!(
            meta.error_kind, "parse_failed",
            "expected parse_failed error_kind, got: {:?}",
            meta.error_kind
        );
    }

    // Target 2: cmd_config_show with a broken YAML file yields parse_failed.
    // `config show` uses `load_config` (the typed serde path), so a valid-YAML
    // but schema-violating file hits a different code path than cmd_config_get.
    // This uses the same syntactically-invalid YAML to ensure the serde_yaml layer
    // rejects it before schema validation is even reached.
    #[test]
    fn cmd_config_show_broken_yaml_yields_parse_failed_error_kind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.yaml");
        std::fs::write(&path, "spec:\n  - : :\n").unwrap();
        let cli = test_cli_for(path);
        let printer = test_printer();

        let err = cmd_config_show(&cli, &printer).unwrap_err();
        let meta = err
            .downcast_ref::<crate::cli::CliErrorMeta>()
            .expect("CliErrorMeta carrier on parse_failed");
        assert_eq!(
            meta.error_kind, "parse_failed",
            "expected parse_failed error_kind, got: {:?}",
            meta.error_kind
        );
    }

    // Target 3: the setter refuses a blocked descent exactly as the read walk
    // does, so a script reading one channel and a person reading the other are
    // told the same thing. `a: 1` is a leaf the schema declares nothing under;
    // `daemon: yes` is a mapping the document got wrong.
    #[test]
    fn walk_yaml_path_mut_under_a_leaf_is_a_missing_key() {
        let mut yaml: serde_yaml::Value = serde_yaml::from_str("a: 1\n").unwrap();
        let err = walk_yaml_path_mut(&mut yaml, "a.b.c").unwrap_err();
        assert_eq!(err.to_string(), "config error: key 'a.b' not found");
    }

    #[test]
    fn walk_yaml_path_mut_at_a_declared_mapping_names_the_shape_it_found() {
        let mut yaml: serde_yaml::Value = serde_yaml::from_str("daemon: yes\n").unwrap();
        let err = walk_yaml_path_mut(&mut yaml, "daemon.reconcile.interval").unwrap_err();
        assert_eq!(
            err.to_string(),
            "'daemon' holds a scalar where a mapping belongs"
        );
    }

    // The root of the walk is `spec` itself, which no segment names.
    #[test]
    fn walk_yaml_path_mut_names_the_root_when_the_document_is_not_a_mapping() {
        let mut yaml: serde_yaml::Value = serde_yaml::from_str("- a\n").unwrap();
        let err = walk_yaml_path_mut(&mut yaml, "profile").unwrap_err();
        assert_eq!(
            err.to_string(),
            "'spec' holds a sequence where a mapping belongs"
        );
    }

    // `daemon: null` is how a serialized `None` section reads back (and how a
    // hand-written bare `daemon:` parses); it is the section being absent, not
    // a scalar standing in the way, so the walk descends through it exactly as
    // it does through a missing key.
    #[test]
    fn walk_yaml_path_mut_descends_through_a_null_section() {
        let mut yaml: serde_yaml::Value = serde_yaml::from_str("daemon: null\n").unwrap();
        {
            let (parent, leaf) =
                walk_yaml_path_mut(&mut yaml, "daemon.reconcile.autoApply").unwrap();
            assert_eq!(leaf, "autoApply");
            parent.insert("autoApply".into(), serde_yaml::Value::Bool(true));
        }
        assert_eq!(
            yaml["daemon"]["reconcile"]["autoApply"],
            serde_yaml::Value::Bool(true)
        );
    }

    #[test]
    fn walk_yaml_path_mut_null_root_becomes_a_mapping() {
        let mut yaml = serde_yaml::Value::Null;
        let (parent, leaf) = walk_yaml_path_mut(&mut yaml, "a.b").unwrap();
        assert_eq!(leaf, "b");
        assert!(parent.is_empty());
    }

    #[test]
    fn walk_yaml_path_null_section_reads_as_key_not_found() {
        let yaml: serde_yaml::Value = serde_yaml::from_str("daemon: null\n").unwrap();
        let err = walk_yaml_path(&yaml, "daemon.reconcile").unwrap_err();
        match err.downcast_ref::<cfgd_core::errors::CfgdError>() {
            Some(cfgd_core::errors::CfgdError::Config(
                cfgd_core::errors::ConfigError::KeyNotFound { key },
            )) => assert_eq!(key, "daemon.reconcile"),
            other => panic!("expected KeyNotFound, got {other:?}"),
        }
    }

    // The command-level shape of the bug: a config `cfgd init --from` rewrote
    // carried `daemon: null`, and `cfgd config set daemon.reconcile.autoApply
    // true` refused to traverse it.
    #[test]
    fn cmd_config_set_writes_through_a_null_section() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfgd.yaml");
        std::fs::write(
            &path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: base\n  daemon: null\n",
        )
        .unwrap();
        let cli = test_cli_for(path.clone());
        let printer = test_printer();

        cmd_config_set(&cli, &printer, "daemon.reconcile.autoApply", "true").unwrap();

        let written: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            written["spec"]["daemon"]["reconcile"]["autoApply"],
            serde_yaml::Value::Bool(true)
        );
    }

    #[test]
    fn cmd_config_set_preserves_modeline_and_banner_without_duplicating() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("cfgd.yaml");
        // Real scaffold writer — emits the editor schema modeline.
        crate::cli::helpers::write_scaffold(
            cfgd_core::config::SchemaDocKind::Config,
            &config_path,
            SAMPLE_CONFIG,
        )
        .unwrap();
        // A user banner added by hand below the modeline.
        let raw = std::fs::read_to_string(&config_path).unwrap();
        let (modeline, body) = raw.split_once('\n').unwrap();
        std::fs::write(&config_path, format!("{modeline}\n# team banner\n{body}")).unwrap();

        let cli = test_cli_for(config_path.clone());
        let printer = test_printer();
        cmd_config_set(&cli, &printer, "theme.name", "nord").unwrap();

        let after = std::fs::read_to_string(&config_path).unwrap();
        let mut lines = after.lines();
        assert_eq!(
            lines.next().unwrap(),
            modeline,
            "modeline must survive the rewrite"
        );
        assert_eq!(
            lines.next().unwrap(),
            "# team banner",
            "user banner must survive the rewrite"
        );
        assert!(after.contains("name: nord"), "mutation must land: {after}");

        // Second rewrite must not duplicate the block.
        cmd_config_set(&cli, &printer, "theme.name", "minimal").unwrap();
        let after2 = std::fs::read_to_string(&config_path).unwrap();
        assert_eq!(after2.matches("# team banner").count(), 1);
        assert_eq!(after2.matches("yaml-language-server").count(), 1);
        assert!(after2.contains("name: minimal"));
    }
    /// The `spec.` prefix the docs and `cfgd explain` print names the same
    /// field on every key verb, so a reader who copies a path out of
    /// `cfgd explain` can paste it into any of the three.
    #[test]
    fn every_config_key_verb_accepts_the_spec_prefix_the_docs_print() {
        let dir = tempfile::tempdir().unwrap();

        // get: the prefixed spelling answers with the same value the bare one
        // does, and the `-o json` envelope keys it the folded way.
        let cli = test_cli_for(write_sample_config(dir.path()));
        let (printer, cap) = Printer::for_test_doc();
        cmd_config_get(&cli, &printer, "spec.theme.name").unwrap();
        drop(printer);
        assert_eq!(cap.human().trim(), "monokai");

        let (printer, cap) = Printer::for_test_doc_with_format(OutputFormat::Json);
        cmd_config_get(&cli, &printer, "spec.output.theme.name").unwrap();
        drop(printer);
        let parsed = cap.json().expect("doc captured json");
        assert_eq!(parsed["key"], "output.theme.name");
        assert_eq!(parsed["value"], "monokai");

        // set: the prefixed key writes the field it names and nothing else.
        let printer = test_printer();
        cmd_config_set(&cli, &printer, "spec.migrationPolicy", "Ignore").unwrap();
        let after = std::fs::read_to_string(&cli.config).unwrap();
        assert!(
            after.contains("  migrationPolicy: Ignore\n"),
            "the prefixed key writes `spec.migrationPolicy`: {after}"
        );
        assert!(
            !after.contains("spec:\n  spec:") && !after.contains("\n  spec:"),
            "nothing named `spec` is written underneath `spec`: {after}"
        );
        assert!(
            after.contains("name: monokai"),
            "the write touches nothing else: {after}"
        );

        // unset: the prefixed key clears the field the bare one addresses.
        cmd_config_unset(&cli, &printer, "spec.theme.name").unwrap();
        let cleared = std::fs::read_to_string(&cli.config).unwrap();
        assert!(
            !cleared.contains("name: monokai"),
            "the prefixed unset clears `spec.output.theme.name`: {cleared}"
        );
    }

    /// Every `cmd_config_*` taking a caller-written key folds the `spec.`
    /// prefix before anything reads it. The population is read off the source
    /// with no hand list, so a fourth key verb joins it by being compiled.
    #[test]
    fn every_config_key_verb_folds_the_spec_prefix_before_it_reads_the_key() {
        use cfgd_core::test_helpers::{calls_free_fn, file_declarations};

        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/cli/config_cmd.rs");
        let declarations = file_declarations(&path);
        let mut verbs = Vec::new();
        let mut missing = Vec::new();
        for (name, owner, code) in &declarations {
            let signature = code.split_once(')').map_or(code.as_str(), |(head, _)| head);
            if owner.is_some()
                || !name.starts_with("cmd_config_")
                || !signature.contains("key: &str")
            {
                continue;
            }
            verbs.push(name.clone());
            if !calls_free_fn(code, "spec_relative_key") {
                missing.push(name.clone());
            }
        }
        assert!(
            verbs.len() >= 3,
            "the walk found {} key verbs: {verbs:?}",
            verbs.len()
        );
        assert!(
            missing.is_empty(),
            "these key verbs never fold the `spec.` prefix, so the spelling the docs print \
             is a usage error there: {missing:?}"
        );
    }

    /// A config document whose `spec` is `spec`.
    fn document_text(spec: &serde_yaml::Value) -> String {
        let body: String = serde_yaml::to_string(spec)
            .unwrap()
            .lines()
            .map(|l| format!("  {l}\n"))
            .collect();
        format!("apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n{body}")
    }

    /// [`document_text`] written to `dir/cfgd.yaml`, returning its path.
    fn document_with_spec(dir: &std::path::Path, spec: &serde_yaml::Value) -> std::path::PathBuf {
        let path = dir.join("cfgd.yaml");
        std::fs::write(&path, document_text(spec)).unwrap();
        path
    }

    /// The refusal meta a `config` verb's error carries.
    fn refusal(err: &anyhow::Error) -> &crate::cli::CliErrorMeta {
        err.downcast_ref::<crate::cli::CliErrorMeta>()
            .unwrap_or_else(|| panic!("a typed refusal, got: {err:#}"))
    }

    /// `spec.migrationPolicy` has a default the load-time gate already runs
    /// under, so `config get` answers it on a document that never names it,
    /// on both channels. A key the schema does not know, and an optional key
    /// with no value, stay the missing key.
    #[test]
    fn cmd_config_get_answers_an_undeclared_migration_policy_with_the_build_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfgd.yaml");
        std::fs::write(
            &path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  daemon:\n    enabled: true\n",
        )
        .unwrap();
        let cli = test_cli_for(path);

        let (printer, cap) = Printer::for_test_doc();
        cmd_config_get(&cli, &printer, "migrationPolicy").unwrap();
        drop(printer);
        assert_eq!(cap.human().trim(), "Prompt");

        let (printer, cap) = Printer::for_test_doc_with_format(OutputFormat::Json);
        cmd_config_get(&cli, &printer, "spec.migrationPolicy").unwrap();
        drop(printer);
        assert_eq!(
            cap.json().expect("doc captured json"),
            serde_json::json!({ "key": "migrationPolicy", "value": "Prompt" })
        );

        for (key, named) in [("nope", "nope"), ("profile", "profile")] {
            let err = cmd_config_get(&cli, &test_printer(), key).unwrap_err();
            let meta = refusal(&err);
            assert_eq!(meta.error_kind, "key_not_found", "{key}: {}", meta.message);
            assert!(
                meta.message.contains(&format!("key '{named}' not found")),
                "{key}: {}",
                meta.message
            );
        }
    }

    /// Every scalar leaf the `Config` schema addresses, asked for on a document
    /// that declares the sections above it and nothing else, answers with the
    /// value the typed config carries there, read off its JSON serialization:
    /// a default where serde fills one, the missing key where the typed value
    /// is null.
    #[test]
    fn every_config_leaf_the_document_leaves_out_answers_with_the_typed_default() {
        let leaves: Vec<Vec<String>> = addressable_config_fields()
            .into_iter()
            .filter(|(_, node)| node.type_desc != "object" && !node.type_desc.starts_with("[]"))
            .map(|(segments, _)| segments)
            .collect();
        let mut answered = Vec::new();
        for segments in &leaves {
            let key = segments.join(".");
            let dir = tempfile::tempdir().unwrap();
            let sections = spec_holding(
                &segments[..segments.len() - 1],
                serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
            );
            let bytes = document_text(&sections);
            let path = dir.path().join("cfgd.yaml");
            std::fs::write(&path, &bytes).unwrap();
            let config = cfgd_core::config::parse_config(&bytes, &path)
                .unwrap_or_else(|e| panic!("{key}: the sections above it parse empty: {e}"));
            let pointer = format!("/{}", segments.join("/"));
            let typed = serde_json::to_value(&config.spec)
                .unwrap()
                .pointer(&pointer)
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let cli = test_cli_for(path);
            let (printer, cap) = Printer::for_test_doc_with_format(OutputFormat::Json);
            let result = cmd_config_get(&cli, &printer, &key);
            drop(printer);
            if typed.is_null() {
                let err = result
                    .err()
                    .unwrap_or_else(|| panic!("{key} holds no typed value, yet `get` answered"));
                assert_eq!(refusal(&err).error_kind, "key_not_found", "{key}");
                continue;
            }
            if let Err(err) = result {
                panic!("{key} carries the typed default {typed}, yet `get` refused: {err:#}");
            }
            assert_eq!(
                cap.json().expect("doc captured json"),
                serde_json::json!({ "key": key, "value": typed }),
                "{key}"
            );
            answered.push(key);
        }
        for named in [
            "migrationPolicy",
            "fileStrategy",
            "daemon.enabled",
            "daemon.reconcile.interval",
            "output.theme.name",
        ] {
            assert!(
                answered.iter().any(|key| key == named),
                "{named} was not answered from its default; answered: {answered:?}"
            );
        }
        assert!(
            answered.len() >= ANSWERED_FLOOR,
            "{} defaulted leaves answered, below the floor of {ANSWERED_FLOOR}: {answered:?}",
            answered.len()
        );
    }

    /// How many leaves the walk above answers from a default today, so a
    /// schema walk that stopped descending fails by count as well as by name.
    const ANSWERED_FLOOR: usize = 35;

    /// A key `nested_output_key` rewrote is refused under the spelling the
    /// caller wrote, by every verb and on every refusal a rewritten key can
    /// earn: a missing key, a shape that blocks the walk, and a path with an
    /// empty segment.
    #[test]
    fn every_key_verb_refuses_a_rewritten_key_by_the_spelling_the_caller_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let bare = document_with_spec(dir.path(), &serde_yaml::from_str("profile: work").unwrap());
        let bare_cli = test_cli_for(bare);
        let shape_dir = tempfile::tempdir().unwrap();
        let blocked = document_with_spec(
            shape_dir.path(),
            &serde_yaml::from_str("output: 5").unwrap(),
        );
        let blocked_cli = test_cli_for(blocked);
        let leaf_dir = tempfile::tempdir().unwrap();
        let leaf = document_with_spec(
            leaf_dir.path(),
            &serde_yaml::from_str("output:\n  theme:\n    name: nord").unwrap(),
        );
        let leaf_cli = test_cli_for(leaf);

        type Verb = fn(&Cli, &Printer, &str) -> anyhow::Result<()>;
        let get: Verb = cmd_config_get;
        let set: Verb = |cli, printer, key| cmd_config_set(cli, printer, key, "dracula");
        let unset: Verb = cmd_config_unset;
        let cases: [(&str, Verb, &Cli, &str, &str, &str); 8] = [
            (
                "get",
                get,
                &bare_cli,
                "theme.name",
                "key_not_found",
                "key 'theme.name' not found",
            ),
            (
                "get",
                get,
                &blocked_cli,
                "theme.name",
                "parse_failed",
                "'output' holds a scalar where a mapping belongs; 'theme.name' is stored at 'output.theme.name'",
            ),
            (
                "get",
                get,
                &bare_cli,
                "theme..name",
                "invalid_value",
                "invalid key path 'theme..name'",
            ),
            (
                "set",
                set,
                &leaf_cli,
                "theme.name.x",
                "key_not_found",
                "key 'theme.name.x' not found",
            ),
            (
                "set",
                set,
                &blocked_cli,
                "theme.name",
                "parse_failed",
                "'output' holds a scalar where a mapping belongs; 'theme.name' is stored at 'output.theme.name'",
            ),
            (
                "set",
                set,
                &bare_cli,
                "theme..name",
                "invalid_value",
                "invalid key path 'theme..name'",
            ),
            (
                "unset",
                unset,
                &leaf_cli,
                "theme.name.x",
                "key_not_found",
                "key 'theme.name.x' not found",
            ),
            (
                "unset",
                unset,
                &blocked_cli,
                "theme.name",
                "parse_failed",
                "'output' holds a scalar where a mapping belongs; 'theme.name' is stored at 'output.theme.name'",
            ),
        ];
        for (verb, run, cli, key, kind, message) in cases {
            let err = run(cli, &test_printer(), key)
                .err()
                .unwrap_or_else(|| panic!("{verb} {key} was not refused"));
            let meta = refusal(&err);
            assert_eq!(meta.error_kind, kind, "{verb} {key}: {}", meta.message);
            assert_eq!(meta.name, key, "{verb} {key}: the -o json name");
            assert!(
                meta.message.contains(message),
                "{verb} {key}: {:?} does not carry {message:?}",
                meta.message
            );
        }
    }
}
