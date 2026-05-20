use serde::Serialize;

use cfgd_core::output::{Doc, Printer as PrinterV2, Role, SectionBuilder, renderer::Table};

// cfgd explain — schema documentation for all resource types
// ---------------------------------------------------------------------------

/// A field in a resource schema.
pub struct SchemaField {
    /// YAML field name (camelCase)
    pub name: &'static str,
    /// Field type description
    pub type_desc: &'static str,
    /// Whether the field is required
    pub required: bool,
    /// Short description
    pub description: &'static str,
    /// Nested fields (for objects)
    pub children: &'static [SchemaField],
}

/// A top-level resource type.
pub struct ResourceSchema {
    /// Display name
    pub name: &'static str,
    /// apiVersion value
    pub api_version: &'static str,
    /// kind value
    pub kind: &'static str,
    /// File location hint
    pub location: &'static str,
    /// Short description
    pub description: &'static str,
    /// Top-level fields under spec (or root for non-KRM)
    pub fields: &'static [SchemaField],
}

// --- Per-schema submodules ---

mod schema_config;
mod schema_config_source;
mod schema_configpolicy;
mod schema_driftalert;
mod schema_machineconfig;
mod schema_module;
mod schema_profile;
mod schema_teamconfig;

#[cfg(test)]
mod tests;

use schema_config::SCHEMA_CONFIG;
use schema_config_source::SCHEMA_CONFIG_SOURCE;
use schema_configpolicy::SCHEMA_CONFIGPOLICY;
use schema_driftalert::SCHEMA_DRIFTALERT;
use schema_machineconfig::SCHEMA_MACHINECONFIG;
use schema_module::SCHEMA_MODULE;
use schema_profile::SCHEMA_PROFILE;
use schema_teamconfig::SCHEMA_TEAMCONFIG;

static ALL_SCHEMAS: &[&ResourceSchema] = &[
    &SCHEMA_MODULE,
    &SCHEMA_PROFILE,
    &SCHEMA_CONFIG,
    &SCHEMA_CONFIG_SOURCE,
    &SCHEMA_MACHINECONFIG,
    &SCHEMA_CONFIGPOLICY,
    &SCHEMA_DRIFTALERT,
    &SCHEMA_TEAMCONFIG,
];

/// Lookup table mapping user-facing names to schemas (case-insensitive).
pub fn find_schema(name: &str) -> Option<&'static ResourceSchema> {
    let lower = name.to_lowercase();
    ALL_SCHEMAS
        .iter()
        .find(|s| {
            s.name.to_lowercase() == lower
                || s.kind.to_lowercase() == lower
                // Additional aliases for discoverability
                || (lower == "source" && s.name == "ConfigSource")
                || (lower == "cfgd-source" && s.name == "ConfigSource")
        })
        .copied()
}

/// Walk a dot-separated field path to find nested fields.
fn resolve_field_path<'a>(
    fields: &'a [SchemaField],
    path_parts: &[&str],
) -> Option<&'a [SchemaField]> {
    if path_parts.is_empty() {
        return Some(fields);
    }
    let target = path_parts[0];
    for field in fields {
        if field.name == target {
            if path_parts.len() == 1 {
                if field.children.is_empty() {
                    // Leaf field — return it as a single-element slice
                    return Some(std::slice::from_ref(field));
                }
                return Some(field.children);
            }
            return resolve_field_path(field.children, &path_parts[1..]);
        }
    }
    None
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExplainOutput {
    pub name: &'static str,
    pub api_version: &'static str,
    pub kind: &'static str,
    pub location: &'static str,
    pub description: &'static str,
    pub fields: Vec<ExplainField>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExplainField {
    pub name: &'static str,
    #[serde(rename = "type")]
    pub type_desc: &'static str,
    pub required: bool,
    pub description: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<ExplainField>,
}

/// Drill-down payload (`cfgd explain <resource>.<field.path>`).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExplainDrilldownOutput {
    pub path: String,
    pub fields: Vec<ExplainField>,
}

fn schema_field_to_explain(field: &SchemaField) -> ExplainField {
    ExplainField {
        name: field.name,
        type_desc: field.type_desc,
        required: field.required,
        description: field.description,
        children: field.children.iter().map(schema_field_to_explain).collect(),
    }
}

fn schema_to_output(schema: &ResourceSchema) -> ExplainOutput {
    ExplainOutput {
        name: schema.name,
        api_version: schema.api_version,
        kind: schema.kind,
        location: schema.location,
        description: schema.description,
        fields: schema.fields.iter().map(schema_field_to_explain).collect(),
    }
}

/// Append a schema field as a Status row, recursively nesting children under a
/// subsection when `recursive` is set. Nested indentation comes from the
/// renderer's section depth — never manual whitespace.
fn append_field(s: SectionBuilder, f: &SchemaField, recursive: bool) -> SectionBuilder {
    let req = if f.required { " (required)" } else { "" };
    let leaf = if !f.children.is_empty() && !recursive {
        " [+]"
    } else {
        ""
    };
    let header = format!("{} <{}>{}{}", f.name, f.type_desc, req, leaf);
    let s = s.status_with(Role::Info, header, |sf| sf.detail(f.description));
    if recursive && !f.children.is_empty() {
        s.subsection(f.name, |sub| {
            f.children
                .iter()
                .fold(sub, |sub, c| append_field(sub, c, true))
        })
    } else {
        s
    }
}

/// Build the `cfgd explain` (no args) Doc — lists all known schemas.
pub fn build_explain_index_doc() -> Doc {
    let schemas: Vec<ExplainOutput> = ALL_SCHEMAS.iter().map(|s| schema_to_output(s)).collect();
    let mut table = Table::new(["NAME", "API/KIND", "LOCATION"]);
    for s in ALL_SCHEMAS {
        table = table.row([
            s.name.to_string(),
            format!("{}/{}", s.api_version, s.kind),
            s.location.to_string(),
        ]);
    }
    Doc::new()
        .heading("Available resource types")
        .table(table)
        .hint("Use 'cfgd explain <resource>' for details")
        .hint("Use 'cfgd explain <resource>.<field>' to drill into a field")
        .hint("Use 'cfgd explain <resource> --recursive' for all fields expanded")
        .with_data(schemas)
}

/// Build the `cfgd explain <resource>` Doc — schema overview + top-level fields.
pub fn build_explain_schema_doc(schema: &ResourceSchema, recursive: bool) -> Doc {
    let output = schema_to_output(schema);
    Doc::new()
        .heading(format!("{} ({})", schema.name, schema.kind))
        .status(Role::Info, schema.description)
        .kv_block([
            ("apiVersion", schema.api_version),
            ("kind", schema.kind),
            ("location", schema.location),
        ])
        .section("FIELDS (under spec)", |s| {
            schema
                .fields
                .iter()
                .fold(s, |s, f| append_field(s, f, recursive))
        })
        .with_data(output)
}

/// Doc emitted before the not-found error bubbles to `main.rs::printer.error`.
/// Carries the structured payload for `-o json` consumers and a hint listing
/// available resource types; the user-visible error string itself is rendered
/// by `main.rs` so it appears exactly once.
pub fn build_explain_not_found_doc(name: &str, available: &[&'static str]) -> Doc {
    Doc::new()
        .hint("Run 'cfgd explain' to see available resource types.")
        .with_data(serde_json::json!({
            "error": "not_found",
            "name": name,
            "available": available,
        }))
}

/// Build the `cfgd explain <resource>.<field.path>` Doc — drill-in view.
pub fn build_explain_drilldown_doc(
    schema: &ResourceSchema,
    field_path: &[&str],
    fields: &[SchemaField],
    recursive: bool,
) -> Doc {
    let path_str = format!(
        "{}.spec.{}",
        schema.name.to_lowercase(),
        field_path.join(".")
    );
    let mut doc = Doc::new().heading(path_str.clone());
    if fields.len() == 1 && fields[0].children.is_empty() {
        let f = &fields[0];
        let req = if f.required { " (required)" } else { "" };
        doc = doc
            .kv("field", f.name)
            .kv("type", format!("{}{}", f.type_desc, req))
            .status(Role::Info, f.description);
    } else {
        doc = doc.section("Fields", |s| {
            fields.iter().fold(s, |s, f| append_field(s, f, recursive))
        });
    }
    doc.with_data(ExplainDrilldownOutput {
        path: path_str,
        fields: fields.iter().map(schema_field_to_explain).collect(),
    })
}

pub(super) fn cmd_explain(
    v2_printer: &PrinterV2,
    resource: Option<&str>,
    recursive: bool,
) -> anyhow::Result<()> {
    let resource = match resource {
        Some(r) => r,
        None => {
            v2_printer.emit(build_explain_index_doc());
            return Ok(());
        }
    };

    let parts: Vec<&str> = resource.split('.').collect();
    let resource_name = parts[0];
    let field_path = &parts[1..];

    let schema = match find_schema(resource_name) {
        Some(s) => s,
        None => {
            let available: Vec<&'static str> = ALL_SCHEMAS.iter().map(|s| s.name).collect();
            v2_printer.emit(build_explain_not_found_doc(resource_name, &available));
            anyhow::bail!(
                "Unknown resource type '{}'. Run 'cfgd explain' to see available types.",
                resource_name
            );
        }
    };

    // The schema lists fields under `spec` directly; `module.spec.packages`
    // resolves identically to `module.packages` so users can paste either form.
    let field_path: &[&str] = if !field_path.is_empty() && field_path[0] == "spec" {
        &field_path[1..]
    } else {
        field_path
    };

    let doc = if field_path.is_empty() {
        build_explain_schema_doc(schema, recursive)
    } else {
        let fields = resolve_field_path(schema.fields, field_path).ok_or_else(|| {
            anyhow::anyhow!(
                "Unknown field path '{}.{}'. Use 'cfgd explain {}' to see available fields.",
                resource_name,
                field_path.join("."),
                resource_name,
            )
        })?;
        build_explain_drilldown_doc(schema, field_path, fields, recursive)
    };
    v2_printer.emit(doc);
    Ok(())
}
