use serde::Serialize;

use cfgd_core::output::{Doc, Printer, SectionBuilder, renderer::Table};
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

/// How many times this process has reflected the full schema set — the
/// observable behind the memo, since a reflection is otherwise invisible except
/// as time spent.
#[cfg(test)]
static SCHEMA_REFLECTIONS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn build_all_schemas() -> Vec<ResourceSchema> {
    #[cfg(test)]
    SCHEMA_REFLECTIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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

/// Find the field a path names, preserving ITS OWN identity — name, type,
/// description — even when it is an object. The counterpart of
/// [`resolve_field_path`], which returns what descends FROM a match (its
/// children, for object drill-in) rather than the match itself: that
/// contract is right for a caller that only wants the children to list, but
/// it means a drill-down VIEW built from it alone shows only the children
/// and silently discards the queried object's own description. Used by
/// [`build_explain_drilldown_doc`] to render that header before
/// auto-expanding the object's fields one level, so `cfgd explain
/// resource.object` never needs `--recursive` to say anything about
/// `object` itself. Traverses non-terminal segments identically to
/// `resolve_field_path` (children first, then each variant's children); it
/// exists only to answer the LAST segment differently.
fn find_field_node<'a>(fields: &'a [FieldNode], path_parts: &[&str]) -> Option<&'a FieldNode> {
    let (target, rest) = path_parts.split_first()?;
    for field in fields {
        if field.name != *target {
            continue;
        }
        if rest.is_empty() {
            return Some(field);
        }
        if !field.children.is_empty() {
            return find_field_node(&field.children, rest);
        }
        for variant in &field.variants {
            if let Some(found) = find_field_node(&variant.children, rest) {
                return Some(found);
            }
        }
        return None;
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

/// The level a field drills into: every accepted shape of a multi-shape
/// union field, then the object's own children.
///
/// A node carries one or the other and never both — a union field has no
/// children of its own (see [`resolve_field_path`]) — so the two are one list
/// rather than two sections. A `variants` entry is a [`FieldNode`] named by
/// its own type description (`string`, `object`, …); see
/// `cfgd_core::schema::union_variants`.
fn drill_level(f: &FieldNode) -> Vec<&FieldNode> {
    f.variants.iter().chain(f.children.iter()).collect()
}

/// One row of a field list: the `name <type>` left column and the description
/// that follows it.
///
/// The name is padded to the level's widest so the type column lines up
/// beneath itself — the row's OWN two-level alignment, inside the single left
/// column the renderer then aligns against the other rows — with the same
/// two-space gap a kv block puts between its key and its value. Char padding
/// is column padding here because a schema field name is an ASCII identifier.
fn field_row(f: &FieldNode, name_width: usize, recursive: bool) -> (String, String) {
    let expandable = !f.children.is_empty() || !f.variants.is_empty();
    let req = if f.required { " (required)" } else { "" };
    let more = if expandable && !recursive { " [+]" } else { "" };
    (
        format!(
            "{:<name_width$}  <{}>{req}{more}",
            f.name,
            f.type_desc,
            name_width = name_width
        ),
        f.description.clone(),
    )
}

/// Append one level of fields as an aligned `name <type> — description` list,
/// followed — when `recursive` — by a subsection per expandable field
/// carrying its own level.
///
/// The rows of a level render as ONE list so the whole level shares one
/// alignment, with the drill-in subsections after it rather than interleaved
/// between the rows they expand. Nested indentation comes from the renderer's
/// section depth, never manual whitespace.
fn append_fields(s: SectionBuilder, fields: &[&FieldNode], recursive: bool) -> SectionBuilder {
    let name_width = fields
        .iter()
        .map(|f| f.name.chars().count())
        .max()
        .unwrap_or(0);
    let s = s.command_list(fields.iter().map(|f| field_row(f, name_width, recursive)));
    if !recursive {
        return s;
    }
    fields.iter().fold(s, |s, f| {
        let level = drill_level(f);
        if level.is_empty() {
            return s;
        }
        s.subsection(f.name.clone(), |sub| append_fields(sub, &level, true))
    })
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
    let fields: Vec<&FieldNode> = schema.fields.iter().collect();
    Doc::new()
        .heading_title("Explain", schema.name.clone())
        .paragraph(schema.description.clone())
        .kv_block([
            ("apiVersion", schema.api_version.as_str()),
            ("kind", schema.kind.as_str()),
            ("location", schema.location.as_str()),
        ])
        .section("Fields (under spec)", |s| {
            append_fields(s, &fields, recursive)
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
    // `find_field_node` looks up the field the FULL path names, independent
    // of `resolve_field_path`'s children-returning contract, so the queried
    // object's own name/type/description renders even when it has several
    // children (a `.modules`-shaped query used to show only `registries`
    // and `security`, with "Module configuration: registries and
    // security." never appearing anywhere). Falling back to `fields` keeps
    // this defensive against a path `resolve_field_path` accepted but this
    // walk did not (should not happen: same schema, same path).
    let node = find_field_node(&schema.fields, field_path);
    let mut doc = Doc::new().heading_title("Explain", path_str.clone());
    if let Some(f) = node {
        doc = doc.paragraph(f.description.clone());
        // Same `<type> (required)` vocabulary the field rows below render, so
        // the drill-in view and the list say a field's type the one way.
        let req = if f.required { " (required)" } else { "" };
        doc = doc.kv("type", format!("<{}>{req}", f.type_desc));
        let variants: Vec<&FieldNode> = f.variants.iter().collect();
        doc = doc.section_if_nonempty("Variants", &variants, |s, variants| {
            append_fields(s, variants, recursive)
        });
        // The object's own fields expand ONE level without `--recursive` —
        // the auto-expand this view exists for; `--recursive` still governs
        // whether each of THOSE fields expands further, via `append_fields`.
        let children: Vec<&FieldNode> = f.children.iter().collect();
        doc = doc.section_if_nonempty("Fields", &children, |s, children| {
            append_fields(s, children, recursive)
        });
    } else {
        let all: Vec<&FieldNode> = fields.iter().collect();
        doc = doc.section("Fields", |s| append_fields(s, &all, recursive));
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
