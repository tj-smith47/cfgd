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
/// source). Draft-07's canonical meta-schema URI uses the `http` scheme;
/// SchemaStore's `cli.js check` validates `$schema` against the canonical
/// dialect URIs and rejects the non-canonical `https` draft-07 form as
/// "Invalid or missing '$schema'" (only draft 2019-09+ / 2020-12 are `https`).
const DRAFT_07: &str = "http://json-schema.org/draft-07/schema#";
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
    if meta.dialect == DRAFT_07 {
        // schemars 1.x emits the draft-2020-12 idiom (`$defs` + `#/$defs/` refs).
        // For documents we declare as draft-07, downgrade to the draft-07 idiom
        // (`definitions` + `#/definitions/`) so the dialect declaration and the
        // keywords agree — what SchemaStore's draft-07 meta-validation wants. The
        // 2020-12 document needs no rewrite: schemars already emits its idiom.
        // Shared with the embedded skill schema so both stay on one idiom.
        cfgd_core::schema::migrate_to_draft_07(&mut value);
    }
    // After the dialect downgrade so the per-file `$schema`/`$id`/`title` win
    // over the generic draft-07 stamp `migrate_to_draft_07` writes.
    stamp_metadata(&mut value, &meta);

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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

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
        // Raw schemars 1.x output keys definitions under `$defs` (draft-2020-12);
        // the draft-07 downgrade to `definitions` happens later in `write_schema`.
        let cfg = schema_value::<CfgdConfig>();
        let overrides = cfg
            .pointer("/$defs/SubscriptionSpec/properties/overrides")
            .expect("SubscriptionSpec.overrides present in schema");
        // An arbitrary-value schema has no "type" / "properties" restriction.
        assert!(
            overrides.get("type").is_none() && overrides.get("properties").is_none(),
            "overrides should be an open/arbitrary schema, got: {overrides}"
        );
    }

    #[test]
    fn draft_07_documents_migrated_to_definitions_idiom() {
        // The config/profile/source documents are published as draft-07, so
        // their generated files must use `definitions` + `#/definitions/` refs,
        // not the draft-2020-12 `$defs` idiom schemars 1.x emits by default.
        let mut value = schema_value::<CfgdConfig>();
        // Pre-migration: schemars 1.x emits the draft-2020-12 idiom.
        assert!(value.get("$defs").is_some());
        assert!(value.get("definitions").is_none());

        cfgd_core::schema::migrate_to_draft_07(&mut value);

        assert!(
            value.get("definitions").is_some(),
            "expected definitions after migration"
        );
        assert!(value.get("$defs").is_none(), "$defs should be renamed away");
        // No `#/$defs/` ref survives.
        let serialized = serde_json::to_string(&value).expect("serialize");
        assert!(
            !serialized.contains("#/$defs/"),
            "no #/$defs/ ref should remain after migration"
        );
        assert!(
            serialized.contains("#/definitions/"),
            "expected #/definitions/ refs after migration"
        );
    }

    #[test]
    fn write_schema_writes_stamped_sorted_json_file() {
        let dir = tempdir().expect("tempdir");
        let meta = SchemaMeta {
            file: "test-config.schema.json",
            dialect: DRAFT_07,
            id: "https://cfgd.io/schemas/test-config.schema.json",
            title: "Test Config",
            description: "Unit test config schema",
        };

        write_schema::<CfgdConfig>(dir.path(), meta).expect("write_schema succeeds");

        let path = dir.path().join("test-config.schema.json");
        let raw = std::fs::read_to_string(&path).expect("file written");

        // Trailing newline requirement.
        assert!(
            raw.ends_with('\n'),
            "schema file must end with a newline, got: {:?}",
            &raw[raw.len().saturating_sub(4)..]
        );

        let v: serde_json::Value = serde_json::from_str(&raw).expect("file is valid JSON");

        assert_eq!(
            v.get("$schema").and_then(|s| s.as_str()),
            Some(DRAFT_07),
            "$schema must be the draft-07 URL"
        );
        assert_eq!(
            v.get("$id").and_then(|s| s.as_str()),
            Some("https://cfgd.io/schemas/test-config.schema.json"),
            "$id must match the canonical id"
        );
        assert_eq!(
            v.get("title").and_then(|s| s.as_str()),
            Some("Test Config"),
            "title must be stamped"
        );
        assert_eq!(
            v.get("description").and_then(|s| s.as_str()),
            Some("Unit test config schema"),
            "description must be stamped"
        );

        // Determinism: a second call must produce byte-identical output.
        let meta2 = SchemaMeta {
            file: "test-config.schema.json",
            dialect: DRAFT_07,
            id: "https://cfgd.io/schemas/test-config.schema.json",
            title: "Test Config",
            description: "Unit test config schema",
        };
        write_schema::<CfgdConfig>(dir.path(), meta2).expect("second write_schema succeeds");
        let raw2 = std::fs::read_to_string(&path).expect("file readable after second write");
        assert_eq!(
            raw, raw2,
            "write_schema must be deterministic (byte-identical on repeat)"
        );
    }

    #[test]
    fn write_schema_2020_12_uses_defs_not_definitions() {
        let dir = tempdir().expect("tempdir");
        let meta = SchemaMeta {
            file: "test-module.schema.json",
            dialect: DRAFT_2020_12,
            id: "https://cfgd.io/schemas/test-module.schema.json",
            title: "Test Module",
            description: "Unit test module schema",
        };

        write_schema::<ModuleDocument>(dir.path(), meta).expect("write_schema 2020-12 succeeds");

        let raw = std::fs::read_to_string(dir.path().join("test-module.schema.json"))
            .expect("file written");
        let v: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");

        // After 2020-12 migration, $defs must exist and definitions must not.
        assert!(
            v.get("$defs").is_some(),
            "2020-12 schema must contain $defs key"
        );
        assert!(
            v.get("definitions").is_none(),
            "2020-12 schema must not contain draft-07 'definitions' key"
        );

        // No #/definitions/ refs must survive; only #/$defs/ refs.
        assert!(
            !raw.contains("#/definitions/"),
            "no #/definitions/ refs should appear in 2020-12 output"
        );
        assert!(
            raw.contains("#/$defs/"),
            "2020-12 output must contain #/$defs/ refs"
        );

        // The $schema stamp must reflect 2020-12.
        assert_eq!(
            v.get("$schema").and_then(|s| s.as_str()),
            Some(DRAFT_2020_12),
            "$schema must be the 2020-12 URL"
        );
    }

    // Test #3 (main_binary_writes_four_schema_files via assert_cmd) is SKIPPED:
    // `assert_cmd` is not a dev-dependency of cfgd-core (confirmed in Cargo.toml).
    // Adding it solely for this test is not warranted; the two unit tests above
    // exercise write_schema end-to-end and cover the same code paths that main()
    // composes. Integration coverage of the binary's arg handling can be added to
    // cfgd-core's integration tests (tests/) if assert_cmd is ever introduced.
}
