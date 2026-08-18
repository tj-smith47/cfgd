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
/// Owned (built from the registry the first time `explain` asks), so its
/// `fields` are the schemars-derived [`FieldNode`] tree rather than a
/// hand-maintained static.
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
            variants: Vec::new(),
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
            variants: Vec::new(),
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
///
/// Reflected at most once per process. Every entry's `field_tree()` re-runs
/// `schemars::schema_for!` over the kind's Rust type and then walks the whole
/// resulting document, and a single `cfgd explain <typo>` used to pay for all
/// nine kinds twice — once for the lookup that misses, once to list the
/// available names in the error. The trees are derived from Rust types compiled
/// into this binary, so nothing a run does can change them.
fn all_schemas() -> &'static [ResourceSchema] {
    static SCHEMAS: std::sync::OnceLock<Vec<ResourceSchema>> = std::sync::OnceLock::new();
    SCHEMAS.get_or_init(build_all_schemas)
}

fn build_all_schemas() -> Vec<ResourceSchema> {
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
pub fn find_schema(name: &str) -> Option<&'static ResourceSchema> {
    let lower = name.to_lowercase();
    // The CRD Module is selectable only via the explicit `module-crd` token, so
    // it must be matched before the generic name/kind pass (which would
    // otherwise return whichever Module is iterated first for a bare query).
    if lower == "module-crd" || lower == "module (crd)" {
        return all_schemas().iter().find(|s| s.name == "Module (CRD)");
    }
    all_schemas().iter().find(|s| {
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
///
/// Each segment matches a field by name within the current candidate list.
/// A matched field with direct `children` (a plain object, or an array of
/// plain objects) resolves the next segment against those. A matched field
/// that is instead a genuine multi-shape union (`variants` non-empty, e.g.
/// a `ScriptEntry` — a bare string or a `{ run, timeout, ... }` object) has
/// NO children of its own, so the next segment is resolved against every
/// variant's own children in turn — the first variant that resolves the
/// REST of the path wins, deterministically, so a caller can drill straight
/// past the variant boundary (`scripts.preApply.run` finds `run` inside the
/// object variant without ever naming `object` in the path).
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
            if !field.children.is_empty() {
                return resolve_field_path(&field.children, &path_parts[1..]);
            }
            for variant in &field.variants {
                if let Some(found) = resolve_field_path(&variant.children, &path_parts[1..]) {
                    return Some(found);
                }
            }
            return None;
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
    /// Every accepted shape of a genuine multi-shape (`oneOf`/`anyOf`) union
    /// field — e.g. a `ScriptEntry` yields a `string` variant and an `object`
    /// variant carrying its own `children`. Empty for every field that is not
    /// such a union. Additive: existing `-o json` consumers reading `children`
    /// see no change.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<ExplainField>,
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
        variants: field.variants.iter().map(schema_field_to_explain).collect(),
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

/// Append a schema field as a Status row, recursively nesting children (and, for
/// a genuine multi-shape union field, every accepted `variants` shape) under a
/// subsection when `recursive` is set. Nested indentation comes from the
/// renderer's section depth — never manual whitespace. A `variants` entry is a
/// [`FieldNode`] named by its own type description (`string`, `object`, …) —
/// see `cfgd_core::schema::union_variants` — so it renders through the same
/// path as a real field, with its own `children` when the shape is an object.
fn append_field(s: SectionBuilder, f: &FieldNode, recursive: bool) -> SectionBuilder {
    let expandable = !f.children.is_empty() || !f.variants.is_empty();
    let req = if f.required { " (required)" } else { "" };
    let leaf = if expandable && !recursive { " [+]" } else { "" };
    let header = format!("{} <{}>{}{}", f.name, f.type_desc, req, leaf);
    let s = s.status_with(Role::Info, header, |sf| sf.detail(f.description.clone()));
    if recursive && expandable {
        s.subsection(f.name.clone(), |sub| {
            let sub = f
                .variants
                .iter()
                .fold(sub, |sub, v| append_field(sub, v, true));
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
    for s in schemas {
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
    if let [f] = fields {
        // resolve_field_path returns a single-element slice in two cases
        // that are indistinguishable from the slice alone: the path names a
        // leaf field with no children (the common case), or it names a
        // parent whose own children list happens to contain exactly one
        // field. Either way, showing that one field's own name/type/
        // description is the correct view — a one-field "Fields" listing
        // and a genuine leaf's own header render identically here — so the
        // force-expand into `Variants`/`Fields` below is unconditionally
        // correct regardless of which case produced it.
        let req = if f.required { " (required)" } else { "" };
        doc = doc
            .kv("field", f.name.clone())
            .kv("type", format!("{}{}", f.type_desc, req))
            .status(Role::Info, f.description.clone());
        doc = doc.section_if_nonempty("Variants", &f.variants, |s, variants| {
            variants
                .iter()
                .fold(s, |s, v| append_field(s, v, recursive))
        });
        doc = doc.section_if_nonempty("Fields", &f.children, |s, children| {
            children
                .iter()
                .fold(s, |s, c| append_field(s, c, recursive))
        });
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
            let available: Vec<String> = all_schemas().iter().map(|s| s.name.clone()).collect();
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
        build_explain_schema_doc(schema, recursive)
    } else {
        let fields = resolve_field_path(&schema.fields, field_path).ok_or_else(|| {
            anyhow::anyhow!(
                "Unknown field path '{}.{}'. Use 'cfgd explain {}' to see available fields.",
                resource_name,
                field_path.join("."),
                resource_name,
            )
        })?;
        build_explain_drilldown_doc(schema, field_path, fields, recursive)
    };
    printer.emit(doc);
    Ok(())
}
