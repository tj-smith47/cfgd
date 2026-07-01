//! Guard: the LLM-facing prose schemas (`generate::schema::get_schema`, fed to
//! the AI generate flow) must document exactly the `spec` fields the structs
//! actually have.
//!
//! The struct-derived JSON schemas in `schemas/*.schema.json` are the SSOT
//! (themselves guarded by `task schema:check`); this test pins the hand-written
//! prose schemas to them, so a struct field rename / add / remove can't silently
//! leave the AI generating configs with stale or missing keys. It already caught
//! `module.platforms`, `config.compliance`, and `config.update` missing from the
//! prose.

use std::collections::BTreeSet;
use std::path::PathBuf;

use cfgd_core::generate::SchemaKind;
use cfgd_core::generate::schema::get_schema;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// Top-level `spec` field names documented in the prose schema for `kind`.
fn prose_spec_keys(kind: SchemaKind) -> BTreeSet<String> {
    let doc: serde_yaml::Value =
        serde_yaml::from_str(get_schema(kind)).expect("prose schema must parse as YAML");
    doc.get("spec")
        .and_then(serde_yaml::Value::as_mapping)
        .map(|m| {
            m.keys()
                .filter_map(|k| k.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Top-level `spec` field names in the struct-derived JSON schema (the SSOT).
fn struct_spec_keys(schema_file: &str) -> BTreeSet<String> {
    let path = repo_root().join("schemas").join(schema_file);
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let v: serde_json::Value = serde_json::from_str(&text).expect("JSON schema must parse");
    let defs = v
        .get("$defs")
        .or_else(|| v.get("definitions"))
        .expect("schema must carry $defs/definitions");
    let spec_ref = v["properties"]["spec"]["$ref"]
        .as_str()
        .expect("schema spec must be a $ref to a definition");
    let def_name = spec_ref
        .rsplit('/')
        .next()
        .expect("ref must have a trailing name");
    defs[def_name]["properties"]
        .as_object()
        .expect("spec definition must have properties")
        .keys()
        .cloned()
        .collect()
}

fn assert_parity(name: &str, kind: SchemaKind, schema_file: &str) {
    let prose = prose_spec_keys(kind);
    let structs = struct_spec_keys(schema_file);
    let stale: Vec<_> = prose.difference(&structs).collect();
    let undocumented: Vec<_> = structs.difference(&prose).collect();
    assert!(
        stale.is_empty() && undocumented.is_empty(),
        "{name} prose schema drifted from {schema_file}.\n  stale (in prose, not struct): {stale:?}\n  undocumented (in struct, not prose): {undocumented:?}\n  → update crates/cfgd-core/src/generate/schema.rs to match the structs"
    );
}

#[test]
fn module_prose_schema_matches_structs() {
    assert_parity("Module", SchemaKind::Module, "cfgd-module.schema.json");
}

#[test]
fn profile_prose_schema_matches_structs() {
    assert_parity("Profile", SchemaKind::Profile, "cfgd-profile.schema.json");
}

#[test]
fn config_prose_schema_matches_structs() {
    assert_parity("Config", SchemaKind::Config, "cfgd-config.schema.json");
}
