//! Editor-config JSON schema generator.
//!
//! Emits the four published SchemaStore schemas
//! (`schemas/{cfgd-config,cfgd-module,cfgd-profile,cfgd-source}.schema.json`)
//! directly from the config document types in `cfgd_core::config`, so the
//! schemas are generated rather than hand-written and can never silently drift
//! from the Rust structs.
//!
//! Mirrors `cfgd-operator`'s `cfgd-gen-crds` bin (`gen_crds.rs`): a standalone
//! build-tool binary that writes serialized schema artifacts. Here it writes
//! files (deterministic, sorted-key JSON) rather than YAML on stdout, because
//! four separate files are produced.
//!
//! # Determinism
//!
//! `serde_json::Value` is backed by `BTreeMap` in this workspace
//! (`preserve_order` is not enabled), so re-serializing each schema through a
//! `Value` sorts every object's keys. Output is therefore byte-stable across
//! runs, which is what `task schema:check` diffs against.
//!
//! # Usage
//!
//! ```text
//! cfgd-gen-schemas [OUTPUT_DIR]   # default: schemas/
//! ```

use std::path::{Path, PathBuf};

use cfgd_core::config::{CfgdConfig, ConfigSourceDocument, ModuleDocument, ProfileDocument};
use schemars::JsonSchema;
use serde_json::{Map, Value};

/// JSON Schema dialect for the three draft-07 documents (config, profile,
/// source). Matches the dialect SchemaStore already serves for these files.
const DRAFT_07: &str = "https://json-schema.org/draft-07/schema#";
/// JSON Schema dialect for the module document. `cfgd-module` is registered
/// with SchemaStore as draft-2020-12 (its catalog entry lives in the
/// `highSchemaVersion` list); keep it so publishing stays consistent.
const DRAFT_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";

/// Per-schema metadata that overrides whatever schemars emits for `$schema`,
/// `$id`, `title`, and `description`, so the generated files keep the stable
/// public identity SchemaStore and editors key off.
struct SchemaMeta {
    /// Output filename (relative to the output dir), e.g. `cfgd-config.schema.json`.
    file: &'static str,
    /// JSON Schema dialect (`$schema`).
    dialect: &'static str,
    /// Canonical `$id`.
    id: &'static str,
    /// Human-readable `title`.
    title: &'static str,
    /// Human-readable `description`.
    description: &'static str,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("schemas"));

    std::fs::create_dir_all(&out_dir)?;

    write_schema::<CfgdConfig>(
        &out_dir,
        SchemaMeta {
            file: "cfgd-config.schema.json",
            dialect: DRAFT_07,
            id: "https://cfgd.io/schemas/cfgd-config.schema.json",
            title: "cfgd Config",
            description: "Root configuration file for cfgd (cfgd.yaml)",
        },
    )?;

    write_schema::<ModuleDocument>(
        &out_dir,
        SchemaMeta {
            file: "cfgd-module.schema.json",
            dialect: DRAFT_2020_12,
            id: "https://cfgd.io/schemas/cfgd-module.schema.json",
            title: "cfgd Module",
            description: "Schema for cfgd Module documents (kind: Module)",
        },
    )?;

    write_schema::<ProfileDocument>(
        &out_dir,
        SchemaMeta {
            file: "cfgd-profile.schema.json",
            dialect: DRAFT_07,
            id: "https://cfgd.io/schemas/cfgd-profile.schema.json",
            title: "cfgd Profile",
            description: "Profile document for cfgd (profiles/*.yaml)",
        },
    )?;

    write_schema::<ConfigSourceDocument>(
        &out_dir,
        SchemaMeta {
            file: "cfgd-source.schema.json",
            dialect: DRAFT_07,
            id: "https://cfgd.io/schemas/cfgd-source.schema.json",
            title: "cfgd ConfigSource",
            description: "ConfigSource manifest published by teams for multi-source config management (cfgd-source.yaml)",
        },
    )?;

    Ok(())
}

/// Generate the schema for `T`, stamp the canonical metadata, and write it to
/// `<out_dir>/<meta.file>` as pretty-printed, sorted-key JSON with a trailing
/// newline.
fn write_schema<T: JsonSchema>(
    out_dir: &Path,
    meta: SchemaMeta,
) -> Result<(), Box<dyn std::error::Error>> {
    let schema = schemars::schema_for!(T);
    // Round-trip through Value so BTreeMap sorts every object's keys.
    let mut value = serde_json::to_value(&schema)?;
    stamp_metadata(&mut value, &meta);
    if meta.dialect == DRAFT_2020_12 {
        // schemars 0.8 emits the draft-07 idiom (`definitions` + `#/definitions/`
        // refs). For the document we declare as draft-2020-12, migrate to the
        // 2020-12 idiom (`$defs` + `#/$defs/`) so the dialect declaration and
        // the keywords agree — what SchemaStore's 2020-12 meta-validation wants.
        migrate_defs_to_2020_12(&mut value);
    }

    let mut json = serde_json::to_string_pretty(&value)?;
    json.push('\n');

    let path = out_dir.join(meta.file);
    std::fs::write(&path, json)?;
    Ok(())
}

/// Overwrite `$schema`, `$id`, `title`, and `description` on the root object.
///
/// schemars emits its own `$schema` (the draft it targets) and a `title` (the
/// Rust type name); both are replaced with the cfgd public identity. `$id` and
/// `description` are not emitted by schemars and are inserted here.
fn stamp_metadata(value: &mut Value, meta: &SchemaMeta) {
    let Value::Object(root) = value else {
        return;
    };
    set_first(root, "$schema", Value::String(meta.dialect.to_string()));
    set_first(root, "$id", Value::String(meta.id.to_string()));
    set_first(root, "title", Value::String(meta.title.to_string()));
    set_first(
        root,
        "description",
        Value::String(meta.description.to_string()),
    );
}

/// Insert or replace `key` on `map`. (BTreeMap ordering is by key, so the final
/// serialized position is fixed regardless of insertion order.)
fn set_first(map: &mut Map<String, Value>, key: &str, val: Value) {
    map.insert(key.to_string(), val);
}

/// Migrate a schema from the draft-07 `definitions` idiom to the draft-2020-12
/// `$defs` idiom: rename the root `definitions` object to `$defs` and rewrite
/// every `#/definitions/...` `$ref` to `#/$defs/...`.
fn migrate_defs_to_2020_12(value: &mut Value) {
    if let Value::Object(root) = value
        && let Some(defs) = root.remove("definitions")
    {
        root.insert("$defs".to_string(), defs);
    }
    rewrite_def_refs(value);
}

/// Recursively rewrite every `$ref` string from the `#/definitions/` prefix to
/// the `#/$defs/` prefix.
fn rewrite_def_refs(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(r)) = map.get_mut("$ref")
                && let Some(rest) = r.strip_prefix("#/definitions/")
            {
                *r = format!("#/$defs/{rest}");
            }
            for v in map.values_mut() {
                rewrite_def_refs(v);
            }
        }
        Value::Array(items) => {
            for v in items {
                rewrite_def_refs(v);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generate `T`'s schema as a `Value` (the same path `write_schema` uses,
    /// minus the metadata stamp) so tests assert against real schemars output.
    fn schema_value<T: JsonSchema>() -> Value {
        serde_json::to_value(schemars::schema_for!(T)).expect("schema serializes to Value")
    }

    /// Collect every object-key that appears anywhere in the schema tree, so a
    /// field-presence assertion doesn't depend on the exact `$ref`/`definitions`
    /// nesting schemars chooses.
    fn all_keys(value: &Value, out: &mut std::collections::BTreeSet<String>) {
        match value {
            Value::Object(map) => {
                for (k, v) in map {
                    out.insert(k.clone());
                    all_keys(v, out);
                }
            }
            Value::Array(items) => {
                for v in items {
                    all_keys(v, out);
                }
            }
            _ => {}
        }
    }

    fn keys_of<T: JsonSchema>() -> std::collections::BTreeSet<String> {
        let mut keys = std::collections::BTreeSet::new();
        all_keys(&schema_value::<T>(), &mut keys);
        keys
    }

    #[test]
    fn source_schema_has_allow_scripts_and_required_and_signed_commits() {
        // SubscriptionSpec.allowScripts + SourceSyncSpec.required live under the
        // root config's `sources[]`; constraints live in the ConfigSource doc.
        let cfg = keys_of::<CfgdConfig>();
        for field in [
            "allowScripts",
            "required",
            "overrides",
            "reject",
            "pinVersion",
        ] {
            assert!(cfg.contains(field), "cfgd-config schema missing {field}");
        }
        let src = keys_of::<ConfigSourceDocument>();
        for field in ["requireSignedCommits", "noScripts", "allowedTargetPaths"] {
            assert!(src.contains(field), "cfgd-source schema missing {field}");
        }
    }

    #[test]
    fn module_schema_has_script_guards_and_package_guards() {
        let m = keys_of::<ModuleDocument>();
        for field in [
            "onlyIf",
            "unless",
            "creates",
            "idleTimeout",
            "continueOnError",
            "interactive",
            "shell",
            "deny",
            "platforms",
            "minVersion",
        ] {
            assert!(m.contains(field), "cfgd-module schema missing {field}");
        }
    }

    #[test]
    fn profile_schema_has_windows_managers_and_script_hooks() {
        let p = keys_of::<ProfileDocument>();
        for field in [
            "winget",
            "chocolatey",
            "scoop",
            "envScope",
            "preApply",
            "postApply",
            "preReconcile",
            "postReconcile",
            "onDrift",
            "onChange",
            "permissions",
            "encryption",
            "envs",
        ] {
            assert!(p.contains(field), "cfgd-profile schema missing {field}");
        }
    }

    #[test]
    fn config_schema_uses_camelcase_not_snakecase() {
        let cfg = keys_of::<CfgdConfig>();
        assert!(cfg.contains("apiVersion"), "expected camelCase apiVersion");
        assert!(
            cfg.contains("fileStrategy"),
            "expected camelCase fileStrategy"
        );
        // The stale hand-written schemas used kebab/snake; ensure no snake leak.
        assert!(
            !cfg.contains("api_version"),
            "schema leaked snake_case api_version"
        );
        assert!(
            !cfg.contains("file_strategy"),
            "schema leaked snake_case file_strategy"
        );
    }

    #[test]
    fn deny_unknown_fields_yields_additional_properties_false() {
        // CfgdConfig derives deny_unknown_fields → schema must close the object.
        let root = schema_value::<CfgdConfig>();
        assert_eq!(
            root.get("additionalProperties"),
            Some(&Value::Bool(false)),
            "root config schema should forbid additional properties"
        );
    }

    #[test]
    fn open_value_fields_schema_as_arbitrary() {
        // overrides/reject (serde_yaml::Value via schemars(with)) must NOT
        // close to a fixed shape — they accept arbitrary YAML.
        let cfg = schema_value::<CfgdConfig>();
        let overrides = cfg
            .pointer("/definitions/SubscriptionSpec/properties/overrides")
            .expect("SubscriptionSpec.overrides present in schema");
        // An arbitrary-value schema has no "type" / "properties" restriction.
        assert!(
            overrides.get("type").is_none() && overrides.get("properties").is_none(),
            "overrides should be an open/arbitrary schema, got: {overrides}"
        );
    }

    #[test]
    fn module_schema_migrated_to_2020_12_defs_idiom() {
        // The module document is published as draft-2020-12, so its generated
        // file must use `$defs` + `#/$defs/` refs, not draft-07 `definitions`.
        let mut value = schema_value::<ModuleDocument>();
        // Pre-migration: schemars 0.8 emits the draft-07 idiom.
        assert!(value.get("definitions").is_some());
        assert!(value.get("$defs").is_none());

        migrate_defs_to_2020_12(&mut value);

        assert!(
            value.get("$defs").is_some(),
            "expected $defs after migration"
        );
        assert!(
            value.get("definitions").is_none(),
            "definitions should be renamed away"
        );
        // No `#/definitions/` ref survives.
        let serialized = serde_json::to_string(&value).expect("serialize");
        assert!(
            !serialized.contains("#/definitions/"),
            "no #/definitions/ ref should remain after migration"
        );
        assert!(
            serialized.contains("#/$defs/"),
            "expected #/$defs/ refs after migration"
        );
    }
}
