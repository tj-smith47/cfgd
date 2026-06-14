use serde::Serialize;

use cfgd_core::output::{Doc, Printer, Role, SectionBuilder, renderer::Table};
use cfgd_core::schema::{FieldNode, KIND_REGISTRY};

// cfgd explain — schema documentation for all resource types
// ---------------------------------------------------------------------------
//
// The field trees and per-kind index are derived at runtime from
// `cfgd_core::schema::KIND_REGISTRY` (the single schemars-sourced registry of
// every local + CRD kind). TeamConfig is the lone exception: it is a Crossplane
// composite resource with no Rust spec type, so its schema is hand-authored
// here rather than derived.

/// A top-level resource type, as `explain` presents it.
///
/// Owned (built per invocation from the registry), so its `fields` are the
/// schemars-derived [`FieldNode`] tree rather than a hand-maintained static.
pub struct ResourceSchema {
    /// Display name (the `kind`, except the CRD `Module` shown as `Module (CRD)`).
    pub name: String,
    /// apiVersion value.
    pub api_version: String,
    /// kind value.
    pub kind: String,
    /// File-location hint.
    pub location: String,
    /// Short description.
    pub description: String,
    /// Top-level fields under spec (or root for non-KRM), schemars-derived.
    pub fields: Vec<FieldNode>,
}

impl ResourceSchema {
    /// The kind's top-level field tree.
    pub fn field_tree(&self) -> Vec<FieldNode> {
        self.fields.clone()
    }
}

#[cfg(test)]
mod tests;

/// Hand-authored TeamConfig schema. TeamConfig is a Crossplane composite
/// resource (XR) with no Rust spec type in the registry, so its field tree is
/// expressed directly rather than derived from schemars.
fn teamconfig_schema() -> ResourceSchema {
    fn leaf(name: &str, type_desc: &str, required: bool, description: &str) -> FieldNode {
        FieldNode {
            name: name.to_string(),
            type_desc: type_desc.to_string(),
            required,
            description: description.to_string(),
            children: Vec::new(),
        }
    }
    fn obj(
        name: &str,
        type_desc: &str,
        required: bool,
        description: &str,
        children: Vec<FieldNode>,
    ) -> FieldNode {
        FieldNode {
            name: name.to_string(),
            type_desc: type_desc.to_string(),
            required,
            description: description.to_string(),
            children,
        }
    }

    ResourceSchema {
        name: "TeamConfig".to_string(),
        api_version: cfgd_core::API_VERSION.to_string(),
        kind: "TeamConfig".to_string(),
        location: "Crossplane Composite Resource (XR)".to_string(),
        description: "Crossplane composite resource for team-level configuration. Fans out to per-user MachineConfig CRDs via composition function.".to_string(),
        fields: vec![
            leaf("team", "string", true, "Team name"),
            leaf("profile", "string", false, "Default profile for team members"),
            obj("source", "object", false, "Team config source", vec![
                leaf("url", "string", true, "Git URL of the team config repo"),
                leaf("branch", "string", false, "Git branch (default: master)"),
            ]),
            obj("modules", "[]object", false, "Modules for the team", vec![
                leaf("name", "string", true, "Module name"),
                obj("sourceRef", "object", false, "Remote module source reference", vec![
                    leaf("url", "string", true, "Git URL"),
                    leaf("ref", "string", false, "Git ref (tag/commit)"),
                ]),
            ]),
            obj("policy", "object", false, "Team policy settings", vec![
                leaf("required", "object", false, "Required configuration items"),
                leaf("recommended", "object", false, "Recommended configuration items"),
                leaf("locked", "object", false, "Locked (non-overridable) items"),
                leaf("requiredModules", "[]string", false, "Modules that must be installed"),
                leaf("recommendedModules", "[]string", false, "Modules that are recommended"),
            ]),
            obj("members", "[]object", false, "Team members", vec![
                leaf("username", "string", true, "Username"),
                leaf("sshPublicKey", "string", false, "SSH public key for enrollment"),
                leaf("profile", "string", false, "Profile override for this member"),
                leaf("hostname", "string", false, "Hostname override"),
            ]),
        ],
    }
}

/// Build the full ordered set of `explain`-known schemas: every
/// [`KIND_REGISTRY`] entry plus the hand-authored TeamConfig. The CRD `Module`
/// (which shares the kind string `"Module"` with the local one) is disambiguated
/// with the display name `"Module (CRD)"`.
fn all_schemas() -> Vec<ResourceSchema> {
    let mut schemas: Vec<ResourceSchema> = KIND_REGISTRY
        .iter()
        .map(|e| {
            let name = if e.crd && e.kind == "Module" {
                "Module (CRD)".to_string()
            } else {
                e.kind.to_string()
            };
            ResourceSchema {
                name,
                api_version: e.api_version.to_string(),
                kind: e.kind.to_string(),
                location: e.location.to_string(),
                description: e.description.to_string(),
                fields: e.field_tree(),
            }
        })
        .collect();
    schemas.push(teamconfig_schema());
    schemas
}

/// Lookup a schema by user-facing name (case-insensitive), kind, or alias.
///
/// A bare `Module`/`module` query always resolves the LOCAL module (`!crd`);
/// the `module-crd` token selects the cluster-side `Module` CRD. The
/// `source`/`cfgd-source` aliases resolve to `ConfigSource`.
pub fn find_schema(name: &str) -> Option<ResourceSchema> {
    let lower = name.to_lowercase();
    // The CRD Module is selectable only via the explicit `module-crd` token, so
    // it must be matched before the generic name/kind pass (which would
    // otherwise return whichever Module is iterated first for a bare query).
    if lower == "module-crd" || lower == "module (crd)" {
        return all_schemas().into_iter().find(|s| s.name == "Module (CRD)");
    }
    all_schemas().into_iter().find(|s| {
        // Never let a bare Module query match the CRD variant.
        if s.name == "Module (CRD)" {
            return false;
        }
        s.name.to_lowercase() == lower
            || s.kind.to_lowercase() == lower
            || (lower == "source" && s.kind == "ConfigSource")
            || (lower == "cfgd-source" && s.kind == "ConfigSource")
            // The root config kind is `Config`; `cfgdconfig` (its Rust type name)
            // and `cfgd` stay accepted for discoverability.
            || ((lower == "cfgdconfig" || lower == "cfgd") && s.kind == "Config")
    })
}

/// Walk a dot-separated field path to find nested fields.
fn resolve_field_path<'a>(fields: &'a [FieldNode], path_parts: &[&str]) -> Option<&'a [FieldNode]> {
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
                return Some(&field.children);
            }
            return resolve_field_path(&field.children, &path_parts[1..]);
        }
    }
    None
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExplainOutput {
    pub name: String,
    pub api_version: String,
    pub kind: String,
    pub location: String,
    pub description: String,
    pub fields: Vec<ExplainField>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExplainField {
    pub name: String,
    #[serde(rename = "type")]
    pub type_desc: String,
    pub required: bool,
    pub description: String,
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

fn schema_field_to_explain(field: &FieldNode) -> ExplainField {
    ExplainField {
        name: field.name.clone(),
        type_desc: field.type_desc.clone(),
        required: field.required,
        description: field.description.clone(),
        children: field.children.iter().map(schema_field_to_explain).collect(),
    }
}

fn schema_to_output(schema: &ResourceSchema) -> ExplainOutput {
    ExplainOutput {
        name: schema.name.clone(),
        api_version: schema.api_version.clone(),
        kind: schema.kind.clone(),
        location: schema.location.clone(),
        description: schema.description.clone(),
        fields: schema.fields.iter().map(schema_field_to_explain).collect(),
    }
}

/// Append a schema field as a Status row, recursively nesting children under a
/// subsection when `recursive` is set. Nested indentation comes from the
/// renderer's section depth — never manual whitespace.
fn append_field(s: SectionBuilder, f: &FieldNode, recursive: bool) -> SectionBuilder {
    let req = if f.required { " (required)" } else { "" };
    let leaf = if !f.children.is_empty() && !recursive {
        " [+]"
    } else {
        ""
    };
    let header = format!("{} <{}>{}{}", f.name, f.type_desc, req, leaf);
    let s = s.status_with(Role::Info, header, |sf| sf.detail(f.description.clone()));
    if recursive && !f.children.is_empty() {
        s.subsection(f.name.clone(), |sub| {
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
    let schemas = all_schemas();
    let outputs: Vec<ExplainOutput> = schemas.iter().map(schema_to_output).collect();
    let mut table = Table::new(["NAME", "API/KIND", "LOCATION"]);
    for s in &schemas {
        table = table.row([
            s.name.clone(),
            format!("{}/{}", s.api_version, s.kind),
            s.location.clone(),
        ]);
    }
    Doc::new()
        .heading("Available resource types")
        .table(table)
        .hint("Use 'cfgd explain <resource>' for details")
        .hint("Use 'cfgd explain <resource>.<field>' to drill into a field")
        .hint("Use 'cfgd explain <resource> --recursive' for all fields expanded")
        .with_data(outputs)
}

/// Build the `cfgd explain <resource>` Doc — schema overview + top-level fields.
pub fn build_explain_schema_doc(schema: &ResourceSchema, recursive: bool) -> Doc {
    let output = schema_to_output(schema);
    Doc::new()
        .heading(format!("{} ({})", schema.name, schema.kind))
        .status(Role::Info, schema.description.clone())
        .kv_block([
            ("apiVersion", schema.api_version.as_str()),
            ("kind", schema.kind.as_str()),
            ("location", schema.location.as_str()),
        ])
        .section("FIELDS (under spec)", |s| {
            schema
                .fields
                .iter()
                .fold(s, |s, f| append_field(s, f, recursive))
        })
        .with_data(output)
}

/// Build the unknown-resource-type error carrying `CliErrorMeta` so the central
/// sink renders it once: the structured payload for `-o json` consumers
/// (`error: not_found`, `name`, `available`) plus a human-mode hint listing
/// available resource types. Callers `return Err(build_explain_not_found_error(...))`.
pub fn build_explain_not_found_error(name: &str, available: &[String]) -> anyhow::Error {
    crate::cli::cli_error_with_hints(
        name,
        "not_found",
        format!("Unknown resource type '{name}'. Run 'cfgd explain' to see available types."),
        serde_json::json!({ "available": available }),
        vec!["Run 'cfgd explain' to see available resource types.".to_string()],
    )
}

/// Build the `cfgd explain <resource>.<field.path>` Doc — drill-in view.
pub fn build_explain_drilldown_doc(
    schema: &ResourceSchema,
    field_path: &[&str],
    fields: &[FieldNode],
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
            .kv("field", f.name.clone())
            .kv("type", format!("{}{}", f.type_desc, req))
            .status(Role::Info, f.description.clone());
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
    printer: &Printer,
    resource: Option<&str>,
    recursive: bool,
) -> anyhow::Result<()> {
    let resource = match resource {
        Some(r) => r,
        None => {
            printer.emit(build_explain_index_doc());
            return Ok(());
        }
    };

    let parts: Vec<&str> = resource.split('.').collect();
    let resource_name = parts[0];
    let field_path = &parts[1..];

    let schema = match find_schema(resource_name) {
        Some(s) => s,
        None => {
            let available: Vec<String> = all_schemas().into_iter().map(|s| s.name).collect();
            return Err(build_explain_not_found_error(resource_name, &available));
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
        build_explain_schema_doc(&schema, recursive)
    } else {
        let fields = resolve_field_path(&schema.fields, field_path).ok_or_else(|| {
            anyhow::anyhow!(
                "Unknown field path '{}.{}'. Use 'cfgd explain {}' to see available fields.",
                resource_name,
                field_path.join("."),
                resource_name,
            )
        })?;
        build_explain_drilldown_doc(&schema, field_path, fields, recursive)
    };
    printer.emit(doc);
    Ok(())
}
