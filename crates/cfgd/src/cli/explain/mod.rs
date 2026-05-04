use serde::Serialize;

use super::*;

// cfgd explain — schema documentation for all resource types
// ---------------------------------------------------------------------------

/// A field in a resource schema.
pub(super) struct SchemaField {
    /// YAML field name (camelCase)
    pub(super) name: &'static str,
    /// Field type description
    pub(super) type_desc: &'static str,
    /// Whether the field is required
    pub(super) required: bool,
    /// Short description
    pub(super) description: &'static str,
    /// Nested fields (for objects)
    pub(super) children: &'static [SchemaField],
}

/// A top-level resource type.
pub(super) struct ResourceSchema {
    /// Display name
    pub(super) name: &'static str,
    /// apiVersion value
    pub(super) api_version: &'static str,
    /// kind value
    pub(super) kind: &'static str,
    /// File location hint
    pub(super) location: &'static str,
    /// Short description
    pub(super) description: &'static str,
    /// Top-level fields under spec (or root for non-KRM)
    pub(super) fields: &'static [SchemaField],
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
fn find_schema(name: &str) -> Option<&'static ResourceSchema> {
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

fn print_field(printer: &Printer, field: &SchemaField, indent: usize, recursive: bool) {
    let prefix = " ".repeat(indent);
    let req = if field.required { " (required)" } else { "" };
    let has_children = if !field.children.is_empty() && !recursive {
        " [+]"
    } else {
        ""
    };
    printer.info(&format!(
        "{}{} <{}>{}{}",
        prefix, field.name, field.type_desc, req, has_children
    ));
    printer.info(&format!("{}  {}", prefix, field.description));

    if recursive && !field.children.is_empty() {
        for child in field.children {
            print_field(printer, child, indent + 2, true);
        }
    }
}

#[derive(Serialize)]
struct ExplainOutput {
    name: &'static str,
    api_version: &'static str,
    kind: &'static str,
    location: &'static str,
    description: &'static str,
    fields: Vec<ExplainField>,
}

#[derive(Serialize)]
struct ExplainField {
    name: &'static str,
    #[serde(rename = "type")]
    type_desc: &'static str,
    required: bool,
    description: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    children: Vec<ExplainField>,
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

pub(super) fn cmd_explain(
    printer: &Printer,
    resource: Option<&str>,
    recursive: bool,
) -> anyhow::Result<()> {
    let resource = match resource {
        Some(r) => r,
        None => {
            if printer.is_structured() {
                let schemas: Vec<ExplainOutput> = ALL_SCHEMAS
                    .iter()
                    .map(|s| ExplainOutput {
                        name: s.name,
                        api_version: s.api_version,
                        kind: s.kind,
                        location: s.location,
                        description: s.description,
                        fields: s.fields.iter().map(schema_field_to_explain).collect(),
                    })
                    .collect();
                printer.write_structured(&schemas);
                return Ok(());
            }
            // List all available resource types
            printer.header("Available resource types");
            let rows: Vec<Vec<String>> = ALL_SCHEMAS
                .iter()
                .map(|s| {
                    vec![
                        s.name.to_string(),
                        format!("{}/{}", s.api_version, s.kind),
                        s.location.to_string(),
                    ]
                })
                .collect();
            printer.table(&["NAME", "API/KIND", "LOCATION"], &rows);
            printer.newline();
            printer.info("Use 'cfgd explain <resource>' for details");
            printer.info("Use 'cfgd explain <resource>.<field>' to drill into a field");
            printer.info("Use 'cfgd explain <resource> --recursive' for all fields expanded");
            return Ok(());
        }
    };

    // Split resource.field.path
    let parts: Vec<&str> = resource.split('.').collect();
    let resource_name = parts[0];
    let field_path = &parts[1..];

    let schema = match find_schema(resource_name) {
        Some(s) => s,
        None => {
            anyhow::bail!(
                "Unknown resource type '{}'. Run 'cfgd explain' to see available types.",
                resource_name
            );
        }
    };

    if printer.is_structured() {
        let output = ExplainOutput {
            name: schema.name,
            api_version: schema.api_version,
            kind: schema.kind,
            location: schema.location,
            description: schema.description,
            fields: schema.fields.iter().map(schema_field_to_explain).collect(),
        };
        printer.write_structured(&output);
        return Ok(());
    }

    // If there's a field path starting with "spec", skip it since we show spec fields directly
    let field_path = if !field_path.is_empty() && field_path[0] == "spec" {
        &field_path[1..]
    } else {
        field_path
    };

    if field_path.is_empty() {
        // Show resource overview + top-level fields
        printer.header(&format!("{} ({})", schema.name, schema.kind));
        printer.info(schema.description);
        printer.newline();
        printer.key_value("apiVersion", schema.api_version);
        printer.key_value("kind", schema.kind);
        printer.key_value("location", schema.location);
        printer.newline();
        printer.subheader("FIELDS (under spec):");
        printer.newline();

        for field in schema.fields {
            print_field(printer, field, 0, recursive);
        }
    } else {
        // Drill into a specific field path
        match resolve_field_path(schema.fields, field_path) {
            Some(fields) => {
                let path_str = format!(
                    "{}.spec.{}",
                    schema.name.to_lowercase(),
                    field_path.join(".")
                );
                printer.header(&path_str);

                if fields.len() == 1 && fields[0].children.is_empty() {
                    // Leaf field
                    let f = &fields[0];
                    let req = if f.required { " (required)" } else { "" };
                    printer.key_value("field", f.name);
                    printer.key_value("type", &format!("{}{}", f.type_desc, req));
                    printer.info(f.description);
                } else {
                    for field in fields {
                        print_field(printer, field, 0, recursive);
                    }
                }
            }
            None => {
                anyhow::bail!(
                    "Unknown field path '{}.{}'. Use 'cfgd explain {}' to see available fields.",
                    resource_name,
                    field_path.join("."),
                    resource_name,
                );
            }
        }
    }

    Ok(())
}
