use serde::Serialize;

use cfgd_core::output::{CommandPair, Doc, KvPair, Printer, SectionBuilder, renderer::Table};
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
    /// Path (plus heading anchor) into the repo's `docs/` tree.
    pub docs: String,
    /// Short description.
    pub description: String,
    /// Top-level fields under spec (or root for non-KRM), schemars-derived.
    pub fields: Vec<FieldNode>,
}

impl ResourceSchema {
    /// The URL the `Docs` row opens, pinned to THIS binary's release tag —
    /// the one derivation both the human row and `-o json`'s `docsUrl` read.
    pub fn docs_url(&self) -> String {
        cfgd_core::config::docs_url(&self.docs, env!("CARGO_PKG_VERSION"))
    }

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
        obj(name, type_desc, required, description, Vec::new())
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
            // TeamConfig's shapes are declared in a Crossplane XRD rather than
            // in a Rust type, so there is no `$defs` entry to name and no
            // schemars enum to enumerate.
            type_name: String::new(),
            enum_values: Vec::new(),
            is_variant: false,
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
        docs: "docs/spec/teamconfig.md#fields".to_string(),
        description: "Crossplane composite resource for team-level configuration. Fans out to per-user MachineConfig CRDs via composition function.".to_string(),
        fields: vec![
            leaf("team", "string", true, "Name of the team this document configures. Used as the prefix of every MachineConfig the composition generates, so it must be a valid DNS label."),
            leaf("profile", "string", false, "Profile every member inherits unless their own entry overrides it. Omitted, each generated MachineConfig carries no profile and the machine keeps whichever profile it already has active."),
            obj("source", "object", false, "Git repository the team's modules and profiles are pulled from. Omitted, members supply their own sources.", vec![
                leaf("url", "string", true, "Clone URL of the team config repository, in any form git accepts (HTTPS or SSH)."),
                leaf("branch", "string", false, "Branch to track. Defaults to the repository's own default branch."),
            ]),
            obj("modules", "[]object", false, "Modules delivered to every member of the team. Each entry becomes a module reference on every generated MachineConfig.", vec![
                leaf("name", "string", true, "Module name, matching the directory under `modules/` in the source repository."),
                obj("sourceRef", "object", false, "Repository this module is fetched from, when it does not live in the team's own source. Omitted, the module resolves against `spec.source`.", vec![
                    leaf("url", "string", true, "Clone URL of the repository holding the module."),
                    leaf("ref", "string", false, "Tag, branch, or commit to pin the module at. Omitted, the repository's default branch is tracked and the module follows it."),
                ]),
            ]),
            obj("policy", "object", false, "What the team mandates, suggests, and forbids members from changing. Enforced by the generated ConfigPolicy.", vec![
                leaf("required", "object", false, "Configuration keys every member must carry, as a mapping of key to required value. A member whose machine disagrees is reported as non-compliant."),
                leaf("recommended", "object", false, "Configuration keys members are advised to carry, in the same mapping shape as `required`. Reported but never enforced."),
                leaf("locked", "object", false, "Configuration keys members may not override, as a mapping of key to the value the team pins. A local override of a locked key is rejected."),
                leaf("requiredModules", "[]string", false, "Modules that must be installed on every member machine. A missing one is a compliance failure."),
                leaf("recommendedModules", "[]string", false, "Modules members are advised to install. Surfaced in compliance reports without failing them."),
            ]),
            obj("members", "[]object", true, "The people on the team. One MachineConfig is generated per entry.", vec![
                leaf("username", "string", true, "Login the member's machine enrolls as. Becomes the name of the generated MachineConfig."),
                leaf("sshPublicKey", "string", false, "Public key the member's enrollment request is verified against. Omitted, the member enrolls through the device-flow instead."),
                leaf("profile", "string", false, "Profile for this member, overriding the team-wide `spec.profile`."),
                leaf("hostname", "string", false, "Hostname to reconcile this member's machine as, when it differs from the machine's own."),
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
                docs: e.docs.to_string(),
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
pub fn resolve_field_path<'a>(
    fields: &'a [FieldNode],
    path_parts: &[&str],
) -> Option<&'a [FieldNode]> {
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
    /// Where this kind is documented in the repo's `docs/` tree. Additive:
    /// a consumer reading only the fields it already knows sees no change.
    pub docs: String,
    /// `docs` as the URL the human row links to, pinned to this binary's
    /// release tag. Additive beside `docs`, which keeps the bare path.
    pub docs_url: String,
    pub description: String,
    pub fields: Vec<ExplainField>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExplainField {
    pub name: String,
    /// The SHAPE word (`object`, `[]object`, `[](string | object)`) — the wire
    /// value, byte-identical to what it has always been. The named type a
    /// human sees instead travels in `typeName`.
    #[serde(rename = "type")]
    pub type_desc: String,
    /// The named `$defs` type behind this field, when it resolved through one
    /// (`ModuleFileEntry`). Absent for an inline anonymous schema. Additive:
    /// `type` is unchanged whether or not this is present.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub type_name: String,
    /// The accepted values of a unit-variant enum field, in declared order.
    /// Absent for every other field.
    #[serde(rename = "enum", skip_serializing_if = "Vec::is_empty")]
    pub enum_values: Vec<String>,
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
        type_name: field.type_name.clone(),
        enum_values: field.enum_values.clone(),
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
        docs: schema.docs.clone(),
        docs_url: schema.docs_url(),
        description: schema.description.clone(),
        fields: schema.fields.iter().map(schema_field_to_explain).collect(),
    }
}

/// The level a field drills into: every accepted shape of a multi-shape
/// union field, then the object's own children sorted by name.
///
/// A node carries one or the other and never both — a union field has no
/// children of its own (see [`resolve_field_path`]) — so the two are one list
/// rather than two sections. A `variants` entry is a [`FieldNode`] named by
/// its own type description (`string`, `object`, …); see
/// `cfgd_core::schema::union_variants`. Variants keep their declared order —
/// they are shapes, not names, and the declared order matches the union's own
/// type description (`string | object`).
fn drill_level(f: &FieldNode) -> Vec<&FieldNode> {
    f.variants
        .iter()
        .chain(sorted_by_name(f.children.iter()))
        .collect()
}

/// A level's fields sorted by name, so a reader scanning for one has an
/// ordering to rely on. Every rendered level sorts through this — the
/// schemars-derived kinds happen to arrive alphabetical, but the hand-authored
/// TeamConfig does not, and nothing guarantees the derived order either.
/// Display-only: the `-o json` payload keeps schema-walk order.
fn sorted_by_name<'a>(fields: impl IntoIterator<Item = &'a FieldNode>) -> Vec<&'a FieldNode> {
    let mut v: Vec<&FieldNode> = fields.into_iter().collect();
    v.sort_by(|a, b| a.name.cmp(&b.name));
    v
}

/// The `<type>` span of a field row — the substring the renderer paints with
/// its own type colour, and the one place the angle brackets are composed.
fn type_span(f: &FieldNode) -> String {
    format!("<{}>", f.displayed_type())
}

/// A field's description with its accepted enum values appended, in the one
/// spelling every surface uses (`enum: Copy, Symlink, Patch`). The description
/// alone for a field that is not a unit-variant enum.
fn described_with_enum(f: &FieldNode) -> String {
    if f.enum_values.is_empty() {
        return f.description.clone();
    }
    let values = enum_line(f);
    if f.description.is_empty() {
        return values;
    }
    format!("{} {values}", f.description)
}

/// The enum vocabulary line itself (`enum: Copy, Symlink, Patch`).
fn enum_line(f: &FieldNode) -> String {
    format!("enum: {}", f.enum_values.join(", "))
}

/// One row of a field list: the `name <type>` left column and the description
/// that follows it.
///
/// The name is padded to the level's widest so the type column lines up
/// beneath itself — the row's OWN two-level alignment, inside the single left
/// column the renderer then aligns against the other rows — with the same
/// two-space gap a kv block puts between its key and its value. Char padding
/// is column padding here because a schema field name is an ASCII identifier.
/// Whether a field drills in — the ONE predicate behind the `[+]` mark and
/// behind the legend explaining it, so a list can never mark a field it then
/// says nothing about (or explain a mark it never printed).
fn is_expandable(f: &FieldNode) -> bool {
    // A shape is not a field: its name is a rendered type, never a path
    // segment, so it never earns the mark that promises one.
    !f.is_variant && (!f.children.is_empty() || !f.variants.is_empty())
}

/// The type span a row renders: `<type>` for a field, nothing for a shape,
/// whose NAME is its type — two columns stating one fact is what
/// `[]string  <[]string>` read as.
fn row_type_span(f: &FieldNode) -> String {
    if f.is_variant {
        String::new()
    } else {
        type_span(f)
    }
}

/// The fields a drill-down lists under `Fields`: the node's own, or — for a
/// union whose ONE object arm is the only shape with fields — that arm's, in
/// the place a plain object would have listed its own. A `Variants` section
/// discloses shapes; it never replaces the field list, and `resolve_field_path`
/// already drills through the arm, so every promoted row is a path segment.
/// Two arms with fields cannot be merged and stay behind `--recursive`.
fn own_fields(f: &FieldNode) -> &[FieldNode] {
    if !f.children.is_empty() {
        return &f.children;
    }
    let mut with_fields = f.variants.iter().filter(|v| !v.children.is_empty());
    match (with_fields.next(), with_fields.next()) {
        (Some(only), None) => &only.children,
        _ => &[],
    }
}

/// The `[+]` legend, or `None` when nothing in `fields` carries the mark.
///
/// `[+]` is minted only by [`field_row`], so a `--recursive` tree — which has
/// already expanded every field — never earns it.
fn expandable_hint(base: &str, fields: &[&FieldNode], recursive: bool) -> Option<String> {
    (!recursive && fields.iter().any(|f| is_expandable(f)))
        .then(|| format!("`cfgd explain {base}.<field>` expands a field marked [+]"))
}

/// The marker `[+]` and the ` (required)` flag each get a COLUMN, not a
/// concatenation.
///
/// The legend tells the reader to scan for `[+]`, and only a column can be
/// scanned: concatenated onto a variable-width type span it landed at six
/// different x positions down eight rows, and ` (required)` — which sits
/// between the type and the mark — moved every mark after it again. Each
/// width is measured over the LEVEL, the same unit the name column is
/// measured over, and a level with nothing required spends no width on the
/// flag, so a list without one is byte-identical to what it always was.
fn field_row(f: &FieldNode, widths: &LevelWidths) -> CommandPair {
    let req = if f.required { " (required)" } else { "" };
    let more = if is_expandable(f) { " [+]" } else { "" };
    let type_span = row_type_span(f);
    let key = format!(
        "{:<name_width$}  {:<type_width$}{:<req_width$}{more}",
        f.name,
        type_span,
        req,
        name_width = widths.name,
        type_width = widths.type_span,
        req_width = widths.required,
    );
    // Nothing follows the last filled column, so its padding is trailing
    // whitespace; `command_list` pads every key to the widest anyway.
    let key = key.trim_end().to_string();
    if f.is_variant {
        CommandPair::new(key, described_with_enum(f))
    } else {
        CommandPair::typed(key, type_span, described_with_enum(f))
    }
}

/// The three column widths a level's rows pad to, measured over that level.
#[derive(Default)]
struct LevelWidths {
    name: usize,
    type_span: usize,
    required: usize,
}

impl LevelWidths {
    /// Measured in chars: a schema field name is an ASCII identifier and a
    /// type description is ASCII too, so char width is column width.
    fn of(fields: &[&FieldNode]) -> Self {
        Self {
            name: fields
                .iter()
                .map(|f| f.name.chars().count())
                .max()
                .unwrap_or(0),
            type_span: fields
                .iter()
                .map(|f| row_type_span(f).chars().count())
                .max()
                .unwrap_or(0),
            required: if fields.iter().any(|f| f.required) {
                " (required)".chars().count()
            } else {
                0
            },
        }
    }
}

/// Append one level of fields.
///
/// Non-recursive: an aligned `name <type> — description` list, with `[+]`
/// marking each field that expands.
///
/// Recursive: one structure tree — `name <type>` per field, each field's own
/// level indented directly beneath its row, descriptions omitted (the
/// `kubectl explain --recursive` shape: the tree is a map of the schema, and
/// a field's documentation is one drill-down away). The whole tree is one
/// `command_list`, so a level's siblings share an alignment; the indentation
/// is tree structure — data, composed into the left column — not a section
/// nesting, so no field name repeats as a heading.
fn append_fields(s: SectionBuilder, fields: &[&FieldNode], recursive: bool) -> SectionBuilder {
    if recursive {
        let mut rows = Vec::new();
        push_tree_rows(&mut rows, fields, 0);
        return s.command_list(rows);
    }
    let widths = LevelWidths::of(fields);
    s.command_list(fields.iter().map(|f| field_row(f, &widths)))
}

/// Collect the recursive structure tree: a `name <type> (required)` row per
/// field, then its drill-in level indented one step deeper. Alignment is per
/// level — siblings pad to their own widest name, measured from the same
/// indent — so a level's type column lines up beneath itself.
///
/// A unit-variant enum's accepted values ride one line below the field, at the
/// depth its children would occupy: the values ARE what the field expands into,
/// and the tree has no other place to put a fact about one row.
fn push_tree_rows(rows: &mut Vec<CommandPair>, fields: &[&FieldNode], depth: usize) {
    let widths = LevelWidths::of(fields);
    for f in fields {
        let req = if f.required { " (required)" } else { "" };
        let type_span = row_type_span(f);
        // The tree carries no `[+]`, so the flag is last and needs no padding
        // of its own — but the type span it follows is variable-width, which
        // is what moved it. Same column, same measurement unit.
        let key = format!(
            "{:indent$}{:<name_width$}  {:<type_width$}{req}",
            "",
            f.name,
            type_span,
            indent = depth * 2,
            name_width = widths.name,
            type_width = widths.type_span,
        );
        rows.push(if f.is_variant {
            CommandPair::new(key.trim_end().to_string(), String::new())
        } else {
            CommandPair::typed(key.trim_end().to_string(), type_span, String::new())
        });
        if !f.enum_values.is_empty() {
            rows.push(CommandPair::new(
                format!("{:indent$}{}", "", enum_line(f), indent = (depth + 1) * 2),
                String::new(),
            ));
        }
        let level = drill_level(f);
        if !level.is_empty() {
            push_tree_rows(rows, &level, depth + 1);
        }
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
        .table(table.without_unfillable_columns())
        .hint("Run `cfgd explain <resource>` for details")
        .hint("Run `cfgd explain <resource>.<field>` to drill into a field")
        .hint("Run `cfgd explain <resource> --recursive` for all fields expanded")
        .with_data(outputs)
}

/// Build the `cfgd explain <resource>` Doc — schema overview + top-level fields.
pub fn build_explain_schema_doc(schema: &ResourceSchema, recursive: bool) -> Doc {
    let output = schema_to_output(schema);
    let fields = sorted_by_name(schema.fields.iter());
    let hint = expandable_hint(&schema.name.to_lowercase(), &fields, recursive);
    let doc = Doc::new()
        .heading_title("Explain", schema.name.clone())
        .paragraph(schema.description.clone())
        // One block, so every row pads to one key column; the pointer is a
        // LINK: the short path on a terminal that can open it, the full URL
        // anywhere else — a partial path is clickable nowhere.
        .kv_rows([
            // name-row-ok: a KRM field name, spelled as the YAML spells it
            KvPair::new("apiVersion", schema.api_version.as_str()),
            // name-row-ok: a KRM field name, spelled as the YAML spells it
            KvPair::new("kind", schema.kind.as_str()),
            KvPair::new("Location", schema.location.as_str()),
            KvPair::linked("Docs", &schema.docs, schema.docs_url()),
        ])
        .section("Fields (under spec)", |s| {
            append_fields(s, &fields, recursive)
        });
    match hint {
        Some(h) => doc.hint(h),
        None => doc,
    }
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
        format!("Unknown resource type '{name}'. Run `cfgd explain` to see available types."),
        serde_json::json!({ "available": available }),
        vec!["Run `cfgd explain` to see available resource types.".to_string()],
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
    // The queried field's own type belongs to the heading — the same
    // `<type> (required)` vocabulary the field rows render, on the line that
    // names the field, so no lone kv row repeats it below.
    // The type reaches the heading through the same named-span slot the field
    // rows use, so a rendered schema type takes the one type colour wherever
    // it appears.
    let mut doc = match node {
        Some(f) => {
            let req = if f.required { " (required)" } else { "" };
            let span = type_span(f);
            Doc::new().heading_title_typed("Explain", format!("{path_str} {span}{req}"), span)
        }
        None => Doc::new().heading_title("Explain", path_str.clone()),
    };
    let mut marked: Vec<&FieldNode> = Vec::new();
    if let Some(f) = node {
        doc = doc.paragraph(described_with_enum(f));
        let variants: Vec<&FieldNode> = f.variants.iter().collect();
        doc = doc.section_if_nonempty("Variants", &variants, |s, variants| {
            append_fields(s, variants, recursive)
        });
        // The object's own fields expand ONE level without `--recursive` —
        // the auto-expand this view exists for; `--recursive` still governs
        // whether each of THOSE fields expands further, via `append_fields`.
        let children = sorted_by_name(own_fields(f).iter());
        doc = doc.section_if_nonempty("Fields", &children, |s, children| {
            append_fields(s, children, recursive)
        });
        // Only a FIELD can be marked: the legend's placeholder must
        // substitute to a path the CLI resolves, and a shape's name is not one.
        marked.extend(children.iter().copied());
        if !recursive && children.is_empty() && f.variants.iter().any(|v| !v.children.is_empty()) {
            doc = doc.hint(format!(
                "`cfgd explain {path_str} --recursive` expands the shapes under Variants"
            ));
        }
    } else {
        let all = sorted_by_name(fields.iter());
        doc = doc.section("Fields", |s| append_fields(s, &all, recursive));
        marked.extend(all.iter().copied());
    }
    if let Some(h) = expandable_hint(&path_str, &marked, recursive) {
        doc = doc.hint(h);
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
                "Unknown field path '{}.{}'. Run `cfgd explain {}` to see available fields.",
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
