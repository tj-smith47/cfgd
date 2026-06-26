//! Unified resource-kind registry.
//!
//! [`KIND_REGISTRY`] is the single source of truth for every cfgd resource kind
//! — both the local YAML document kinds (`Module`, `Profile`, `ConfigSource`,
//! `Config`) and the cluster-side CRD kinds delivered by the [`cfgd_crd`] crate
//! (`MachineConfig`, `ConfigPolicy`, `ClusterConfigPolicy`, `DriftAlert`, and the
//! CRD `Module`). Each [`KindEntry`] carries a `schema_fn` that returns the
//! kind's `schemars`-derived [`schemars::Schema`], so `explain`, `validate`, and
//! the skill installer all read schemas from one place and can never drift apart.
//!
//! The CRD half of the registry is compiled behind the default-on `crd` Cargo
//! feature. Consumers that never touch Kubernetes resources (notably the CSI
//! node plugin) depend on `cfgd-core` with `default-features = false` to keep
//! the heavy `kube`/`k8s-openapi` stack out of their binary.

pub mod snapshot;

use schemars::{Schema, schema_for};
use serde_json::Value;

/// JSON Pointer prefix schemars 1.x uses for definition `$ref`s under the
/// default draft-2020-12 settings (`#/$defs/<Name>`). Earlier schemars releases
/// used draft-07's `#/definitions/<Name>`; both are recognized so the walk keeps
/// resolving refs if the generator's draft ever changes.
const DEFS_REF_PREFIXES: [&str; 2] = ["#/$defs/", "#/definitions/"];

/// The JSON Pointer schemars 1.x emits for a type that recursively references
/// the schema's own root (e.g. a self-referential `Box<Self>`/`Vec<Self>`
/// field). schemars 0.8 instead minted a named `#/definitions/<Self>` ref; under
/// 1.x the root type is not duplicated into the definitions map, so this bare
/// fragment must resolve back to the root schema.
const ROOT_REF: &str = "#";

/// Root schema plus its definitions map, threaded through the walk so a `$ref`
/// resolves whether it targets a named definition (`#/$defs/<Name>`) or the
/// document root (`#`).
#[derive(Clone, Copy)]
struct SchemaCtx<'a> {
    /// The full document root schema — the resolution target for a bare `#` ref.
    root: &'a Value,
    /// The root's definitions object (`$defs`/`definitions`), keyed by name.
    defs: &'a serde_json::Map<String, Value>,
}

/// A field in a resource schema, resolved from the kind's JSON schema.
///
/// Mirrors the shape `explain` renders: a YAML field name, a cfgd type
/// description (`[]string`, `object`, `string`, …), whether the field is
/// required, its description, and any nested object fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldNode {
    /// YAML field name (camelCase).
    pub name: String,
    /// cfgd type description, e.g. `[]string`, `object`, `string`.
    pub type_desc: String,
    /// Whether the field is required.
    pub required: bool,
    /// Short description from the schema (rustdoc on the source field).
    pub description: String,
    /// Nested fields, for object-typed fields.
    pub children: Vec<FieldNode>,
}

/// One resource kind in the unified registry.
///
/// `schema_fn` wraps `schemars::schema_for!` for the kind's spec type, so the
/// registry never holds a stale schema — it regenerates from the live Rust type
/// on every call.
pub struct KindEntry {
    /// `kind` value as it appears in a document or CRD (e.g. `Module`).
    pub kind: &'static str,
    /// `apiVersion` value for documents of this kind.
    pub api_version: &'static str,
    /// File-location hint shown by `explain` (where users author this kind).
    pub location: &'static str,
    /// Short human description of the kind.
    pub description: &'static str,
    /// `true` for cluster-side CRD kinds, `false` for local YAML document kinds.
    /// Discriminates the CRD `Module` from the local `Module` (both share the
    /// `kind` string `"Module"`).
    pub crd: bool,
    /// Returns the kind's `schemars`-derived schema.
    pub schema_fn: fn() -> Schema,
    /// Validate a full YAML document of this kind, returning the offending
    /// messages on failure. Local kinds deserialize into their document type
    /// (leaning on `deny_unknown_fields`) and reject an unknown `apiVersion`;
    /// CRD kinds deserialize the `spec` into the matching `cfgd_crd::*Spec`.
    pub validate_fn: fn(&str) -> Result<(), Vec<String>>,
}

impl KindEntry {
    /// Resolve this kind's schema into a [`FieldNode`] tree (top-level `spec`
    /// fields, with nested object fields recursed).
    pub fn field_tree(&self) -> Vec<FieldNode> {
        field_tree_from_schema(&(self.schema_fn)())
    }

    /// Serialize this kind's schema as a compact JSON string. Empty on
    /// serialization failure (schemars schemas are infallibly serializable, so
    /// this never observably empties in practice).
    ///
    /// Compact form: consumed by the embedded [`snapshot::SchemaSnapshot`], so
    /// keep it one-line to avoid bloating the binary. For a human-readable
    /// diffable form (the golden schema gate), use [`KindEntry::pretty_schema`].
    ///
    /// Emitted as draft-07 (via [`migrate_to_draft_07`]) with whitespace-collapsed
    /// descriptions (via [`normalize_descriptions`]), so the embedded skill
    /// schema stays consistent with the published draft-07 editor schemas and
    /// carries the same single-line descriptions the `explain` walk shows.
    pub fn json_schema(&self) -> String {
        serde_json::to_string(&self.canonical_schema_value()).unwrap_or_default()
    }

    /// Serialize this kind's schema as a pretty-printed JSON string. Empty on
    /// serialization failure (schemars schemas are infallibly serializable, so
    /// this never observably empties in practice).
    ///
    /// Deterministic: this workspace serializes through `serde_json`'s default
    /// `BTreeMap`-backed map (`preserve_order` is off), so keys are sorted and
    /// the output is stable across runs. This is the form the committed golden
    /// snapshots use, so a CI diff pinpoints exactly which schema field changed.
    pub fn pretty_schema(&self) -> String {
        serde_json::to_string_pretty(&self.canonical_schema_value()).unwrap_or_default()
    }

    /// Schema as a `Value` in cfgd's canonical published form: draft-07 dialect
    /// and definition idiom with whitespace-collapsed descriptions. The single
    /// transform behind both [`KindEntry::json_schema`] and
    /// [`KindEntry::pretty_schema`].
    fn canonical_schema_value(&self) -> Value {
        let mut value =
            serde_json::to_value((self.schema_fn)()).unwrap_or(Value::Object(Default::default()));
        normalize_descriptions(&mut value);
        migrate_to_draft_07(&mut value);
        value
    }
}

/// Parse `yaml` and reject an unrecognized `apiVersion`, returning the parsed
/// value so the caller reuses it without a second parse.
fn check_api_version(yaml: &str) -> Result<serde_yaml::Value, Vec<String>> {
    let value: serde_yaml::Value =
        serde_yaml::from_str(yaml).map_err(|e| vec![format!("YAML syntax error: {e}")])?;
    if let Some(av) = value.get("apiVersion").and_then(|v| v.as_str()) {
        crate::config::validate_api_version(av).map_err(|e| vec![e.to_string()])?;
    }
    Ok(value)
}

/// Deserialize a full local document into `D`, rejecting unknown fields (every
/// local document type carries `deny_unknown_fields`) and an unrecognized
/// `apiVersion`. The single error is wrapped in a `Vec` so it joins the
/// registry's uniform `Result<(), Vec<String>>` validation contract.
fn validate_local<D: serde::de::DeserializeOwned>(yaml: &str) -> Result<(), Vec<String>> {
    serde_yaml::from_str::<D>(yaml).map_err(|e| vec![e.to_string()])?;
    check_api_version(yaml)?;
    Ok(())
}

/// Validate a CRD document by deserializing its `spec` into `S`, then running
/// `S`'s cross-field [`cfgd_crd::Validatable::validate`]. CRD specs intentionally
/// omit `deny_unknown_fields` (schemars maps that to `additionalProperties:
/// false`, which Kubernetes rejects for structural schemas), so the type check
/// confirms the spec is well-typed and the `apiVersion` is recognized without
/// the strict-field guard. The cross-field rules are the SAME impl the admission
/// webhook enforces, so a violation rejected at admission is rejected identically
/// here.
#[cfg(feature = "crd")]
fn validate_crd_spec<S: serde::de::DeserializeOwned + cfgd_crd::Validatable>(
    yaml: &str,
) -> Result<(), Vec<String>> {
    let value = check_api_version(yaml)?;
    let spec = value
        .get("spec")
        .cloned()
        .unwrap_or(serde_yaml::Value::Null);
    let spec: S = serde_yaml::from_value(spec).map_err(|e| vec![e.to_string()])?;
    spec.validate()
}

/// Every cfgd resource kind. Local kinds derive their schema from the local
/// config structs; CRD kinds (behind the `crd` feature) derive theirs from the
/// `cfgd_crd::*Spec` types, so webhook and CLI validate against one schema.
pub static KIND_REGISTRY: &[KindEntry] = &[
    KindEntry {
        kind: "Module",
        api_version: crate::API_VERSION,
        location: "modules/<name>/module.yaml",
        description: "A reusable unit of packages, files, scripts, and environment.",
        crd: false,
        schema_fn: || schema_for!(crate::config::ModuleSpec),
        validate_fn: validate_local::<crate::config::ModuleDocument>,
    },
    KindEntry {
        kind: "Profile",
        api_version: crate::API_VERSION,
        location: "profiles/<name>.yaml",
        description: "A composable layer of modules, packages, files, and settings.",
        crd: false,
        schema_fn: || schema_for!(crate::config::ProfileSpec),
        validate_fn: validate_local::<crate::config::ProfileDocument>,
    },
    KindEntry {
        kind: "ConfigSource",
        api_version: crate::API_VERSION,
        location: "cfgd-source.yaml",
        description: "A published source of modules and profiles for multi-source config.",
        crd: false,
        schema_fn: || schema_for!(crate::config::ConfigSourceSpec),
        validate_fn: validate_local::<crate::config::ConfigSourceDocument>,
    },
    KindEntry {
        kind: "Config",
        api_version: crate::API_VERSION,
        location: "cfgd.yaml",
        description: "The root cfgd configuration: active profile, sources, daemon, theme.",
        crd: false,
        schema_fn: || schema_for!(crate::config::CfgdConfig),
        validate_fn: validate_local::<crate::config::CfgdConfig>,
    },
    #[cfg(feature = "crd")]
    KindEntry {
        kind: "MachineConfig",
        api_version: crate::API_VERSION,
        location: "MachineConfig CRD (cfgd.io/v1alpha1)",
        description: "Per-machine desired state reconciled by the cfgd operator.",
        crd: true,
        schema_fn: || schema_for!(cfgd_crd::MachineConfigSpec),
        validate_fn: validate_crd_spec::<cfgd_crd::MachineConfigSpec>,
    },
    #[cfg(feature = "crd")]
    KindEntry {
        kind: "ConfigPolicy",
        api_version: crate::API_VERSION,
        location: "ConfigPolicy CRD (cfgd.io/v1alpha1)",
        description: "Namespace-scoped policy of required modules, packages, and settings.",
        crd: true,
        schema_fn: || schema_for!(cfgd_crd::ConfigPolicySpec),
        validate_fn: validate_crd_spec::<cfgd_crd::ConfigPolicySpec>,
    },
    #[cfg(feature = "crd")]
    KindEntry {
        kind: "ClusterConfigPolicy",
        api_version: crate::API_VERSION,
        location: "ClusterConfigPolicy CRD (cfgd.io/v1alpha1)",
        description: "Cluster-scoped policy fanned out across selected namespaces.",
        crd: true,
        schema_fn: || schema_for!(cfgd_crd::ClusterConfigPolicySpec),
        validate_fn: validate_crd_spec::<cfgd_crd::ClusterConfigPolicySpec>,
    },
    #[cfg(feature = "crd")]
    KindEntry {
        kind: "DriftAlert",
        api_version: crate::API_VERSION,
        location: "DriftAlert CRD (cfgd.io/v1alpha1)",
        description: "A recorded drift event between desired and observed machine state.",
        crd: true,
        schema_fn: || schema_for!(cfgd_crd::DriftAlertSpec),
        validate_fn: validate_crd_spec::<cfgd_crd::DriftAlertSpec>,
    },
    #[cfg(feature = "crd")]
    KindEntry {
        kind: "Module",
        api_version: crate::API_VERSION,
        location: "Module CRD (cfgd.io/v1alpha1)",
        description: "Cluster-side Module CRD: an OCI-packaged module injected via CSI.",
        crd: true,
        schema_fn: || schema_for!(cfgd_crd::ModuleSpec),
        validate_fn: validate_crd_spec::<cfgd_crd::ModuleSpec>,
    },
];

/// Walk a `schemars` [`Schema`] into a [`FieldNode`] tree.
///
/// Reads the root object's `properties` (skipping the KRM envelope keys
/// `apiVersion`/`kind`/`metadata`), descending into the `spec` object so the
/// tree presents authoring fields directly. `$ref`s are resolved against the
/// schema's definitions (`$defs` under schemars 1.x); nested object fields
/// recurse; array element types are unwrapped to a `[]<inner>` type description.
/// Required-ness and descriptions come from the schema. Pure — no I/O.
pub fn field_tree_from_schema(root: &Schema) -> Vec<FieldNode> {
    let root = root.as_value();
    let defs = definitions(root);
    let ctx = SchemaCtx { root, defs };
    let mut visited = std::collections::BTreeSet::new();
    let top = object_properties(root);
    // KRM document schemas (Config) wrap authoring fields under `spec`; CRD and
    // bare-spec schemas already start at the spec object. Descend into `spec`
    // when present so every kind presents its authoring fields uniformly.
    if let Some((_, spec_schema)) = top.iter().find(|(name, _)| name.as_str() == "spec") {
        let descent = RefDescent::enter(spec_schema, &mut visited);
        let resolved = resolve_ref(spec_schema, ctx);
        let props = object_properties(&resolved);
        let fields = fields_from_properties(&props, &required_set(&resolved), ctx, &mut visited);
        descent.leave(&mut visited);
        return fields;
    }
    fields_from_properties(&top, &required_set(root), ctx, &mut visited)
}

/// The schema's definitions object — schemars 1.x emits `$defs`; older drafts
/// used `definitions`. Returns an empty map when neither is present.
fn definitions(root: &Value) -> &serde_json::Map<String, Value> {
    static EMPTY: std::sync::LazyLock<serde_json::Map<String, Value>> =
        std::sync::LazyLock::new(serde_json::Map::new);
    root.as_object()
        .and_then(|o| o.get("$defs").or_else(|| o.get("definitions")))
        .and_then(Value::as_object)
        .unwrap_or(&EMPTY)
}

/// Tracks one `$ref` name on the current descent path so a self-referential
/// schema (a type whose field `$ref`s back to itself, directly or through a
/// `Vec`/`Box`) stops descending instead of recursing forever. Removing the
/// name on the way back up renders the field tree as a tree, not a collapsed
/// DAG: sibling branches that legitimately reference the same type still
/// expand.
struct RefDescent {
    /// The `$ref` name to retire on `leave`, set only when this descent is the
    /// one that inserted it. `None` for an inline (ref-less) schema or for a
    /// re-entry into an already-tracked name (the outer descent owns removal).
    owned: Option<String>,
    /// `false` only when the schema `$ref`s a name already on the descent path
    /// — a cycle the caller must not recurse into.
    safe: bool,
}

impl RefDescent {
    /// Record the schema's `$ref` target (if any) as on the descent path.
    fn enter(schema: &Value, visited: &mut std::collections::BTreeSet<String>) -> Self {
        match ref_name(schema) {
            // Inline schema: always safe, nothing to track.
            None => RefDescent {
                owned: None,
                safe: true,
            },
            // First time on this path: track it and allow descent.
            Some(name) if visited.insert(name.clone()) => RefDescent {
                owned: Some(name),
                safe: true,
            },
            // Already on the path: a cycle — do not descend, do not own removal.
            Some(_) => RefDescent {
                owned: None,
                safe: false,
            },
        }
    }

    /// Whether descending into this schema's children is safe (not a cycle).
    fn safe(&self) -> bool {
        self.safe
    }

    /// Retire the `$ref` name if this descent owns it.
    fn leave(self, visited: &mut std::collections::BTreeSet<String>) {
        if let Some(name) = self.owned {
            visited.remove(&name);
        }
    }
}

/// The cycle-tracking key for a schema's `$ref` target: the definition name for
/// a `#/$defs/<Name>` (or legacy `#/definitions/<Name>`) ref, the literal `#`
/// for a root self-reference, or `None` for an inline schema carrying no `$ref`.
/// Both ref forms must be tracked so a self-referential type — whether schemars
/// names it in the definitions map or points it at the root — stops descending
/// instead of recursing forever.
fn ref_name(schema: &Value) -> Option<String> {
    let reference = schema.as_object()?.get("$ref")?.as_str()?;
    if reference == ROOT_REF {
        return Some(ROOT_REF.to_string());
    }
    DEFS_REF_PREFIXES
        .iter()
        .find_map(|prefix| reference.strip_prefix(prefix))
        .map(str::to_string)
}

/// Resolve a `$ref` to its target schema: a named definition against the root's
/// definitions map, or the document root for a bare `#`. Returns the input
/// unchanged when it carries no `$ref` or the target is missing (graceful, no
/// panic).
fn resolve_ref(schema: &Value, ctx: SchemaCtx) -> Value {
    match ref_name(schema) {
        Some(name) if name == ROOT_REF => ctx.root.clone(),
        Some(name) => ctx
            .defs
            .get(&name)
            .cloned()
            .unwrap_or_else(|| schema.clone()),
        None => schema.clone(),
    }
}

/// Extract the `(name, schema)` pairs from an object schema's `properties`.
fn object_properties(schema: &Value) -> Vec<(String, Value)> {
    schema
        .as_object()
        .and_then(|o| o.get("properties"))
        .and_then(Value::as_object)
        .map(|props| props.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default()
}

/// The set of required field names declared on an object schema.
fn required_set(schema: &Value) -> std::collections::BTreeSet<String> {
    schema
        .as_object()
        .and_then(|o| o.get("required"))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Build [`FieldNode`]s for every property of a (already `$ref`-resolved)
/// object schema. `visited` carries the `$ref` names on the current descent
/// path for cycle protection.
fn object_fields(
    schema: &Value,
    ctx: SchemaCtx,
    visited: &mut std::collections::BTreeSet<String>,
) -> Vec<FieldNode> {
    let props = object_properties(schema);
    fields_from_properties(&props, &required_set(schema), ctx, visited)
}

fn fields_from_properties(
    props: &[(String, Value)],
    required: &std::collections::BTreeSet<String>,
    ctx: SchemaCtx,
    visited: &mut std::collections::BTreeSet<String>,
) -> Vec<FieldNode> {
    let mut fields: Vec<FieldNode> = props
        .iter()
        .filter(|(name, _)| !matches!(name.as_str(), "apiVersion" | "kind" | "metadata" | "status"))
        .map(|(name, schema)| field_node(name, schema, required.contains(name), ctx, visited))
        .collect();
    fields.sort_by(|a, b| a.name.cmp(&b.name));
    fields
}

/// Build a single [`FieldNode`] from a property's schema, resolving `$ref`,
/// mapping its type, and recursing into nested object fields. Descending into a
/// `$ref` already on the path renders it as a leaf (its type description) rather
/// than recursing, so a self-referential schema terminates.
fn field_node(
    name: &str,
    schema: &Value,
    required: bool,
    ctx: SchemaCtx,
    visited: &mut std::collections::BTreeSet<String>,
) -> FieldNode {
    let unwrapped = unwrap_single_subschema(schema);
    let descent = RefDescent::enter(&unwrapped, visited);
    let resolved = resolve_ref(&unwrapped, ctx);
    let description = schema_description(schema)
        .or_else(|| schema_description(&unwrapped))
        .or_else(|| schema_description(&resolved))
        .unwrap_or_default();
    let type_desc = type_description(&resolved, ctx, visited);
    // Children come from the field's own object properties, or — for an array
    // field — from its element type's object properties so `[]object` entries
    // stay drillable (e.g. `packages[].name`). A `$ref` re-entry (cycle) stops
    // here, emitting the field as a leaf.
    let children = if !descent.safe() {
        Vec::new()
    } else if is_object(&resolved) {
        object_fields(&resolved, ctx, visited)
    } else {
        array_element_fields(&resolved, ctx, visited)
    };
    descent.leave(visited);
    FieldNode {
        name: name.to_string(),
        type_desc,
        required,
        description,
        children,
    }
}

/// Unwrap a schema that wraps a single subschema via `allOf`/`anyOf`/`oneOf`
/// (with at most an accompanying `null`), as `schemars` emits for an
/// `Option<T>` whose `T` is a `$ref`. Returns the inner schema so its `$ref`
/// resolves and its object fields recurse; returns the input unchanged when it
/// is not such a single-subschema wrapper.
fn unwrap_single_subschema(schema: &Value) -> Value {
    let Some(obj) = schema.as_object() else {
        return schema.clone();
    };
    // A direct `$ref` or inline object/array needs no unwrapping.
    if obj.contains_key("$ref") || obj.contains_key("properties") || obj.contains_key("items") {
        return schema.clone();
    }
    let variants = ["allOf", "anyOf", "oneOf"]
        .iter()
        .find_map(|key| obj.get(*key))
        .and_then(Value::as_array);
    let Some(variants) = variants else {
        return schema.clone();
    };
    let non_null: Vec<&Value> = variants.iter().filter(|s| !is_null_schema(s)).collect();
    match non_null.as_slice() {
        [single] => (*single).clone(),
        _ => schema.clone(),
    }
}

/// True for the schemars `null` variant emitted in an `Option<T>`'s `anyOf`
/// (`{"type": "null"}`).
fn is_null_schema(schema: &Value) -> bool {
    matches!(
        schema.as_object().and_then(|o| o.get("type")),
        Some(Value::String(t)) if t == "null"
    )
}

/// For an array (resolved) schema whose element type is an object, return the
/// element's object fields so `[]object` entries stay drillable. Returns an
/// empty vec for non-arrays or arrays of scalars. Guards the element `$ref`
/// against a cycle.
fn array_element_fields(
    schema: &Value,
    ctx: SchemaCtx,
    visited: &mut std::collections::BTreeSet<String>,
) -> Vec<FieldNode> {
    let Some(item) = array_item(schema) else {
        return Vec::new();
    };
    let item = unwrap_single_subschema(item);
    let descent = RefDescent::enter(&item, visited);
    let resolved = resolve_ref(&item, ctx);
    let fields = if descent.safe() && is_object(&resolved) {
        object_fields(&resolved, ctx, visited)
    } else {
        Vec::new()
    };
    descent.leave(visited);
    fields
}

/// The element schema of an array schema's `items`. Handles both the single-
/// schema form (`"items": {…}`) and the tuple form (`"items": [{…}, …]`,
/// returning the first), mirroring schemars' `SingleOrVec`.
fn array_item(schema: &Value) -> Option<&Value> {
    let items = schema.as_object()?.get("items")?;
    match items {
        Value::Array(items) => items.first(),
        other => Some(other),
    }
}

/// Pull a `description` out of a schema, if present. schemars 1.x emits the
/// `description` keyword inline on the schema object (draft-2020-12), not nested
/// under a `metadata` wrapper as schemars 0.8 did.
///
/// The string is whitespace-collapsed via [`collapse_ws`]: schemars 1.x copies
/// the rustdoc doc-comment verbatim (preserving the author's hard line wraps),
/// whereas 0.8 collapsed runs of whitespace to single spaces. Collapsing here
/// restores the pre-1.x single-line description and lets the renderer own its
/// own wrapping.
fn schema_description(schema: &Value) -> Option<String> {
    schema
        .as_object()
        .and_then(|o| o.get("description"))
        .and_then(Value::as_str)
        .map(collapse_ws)
        .filter(|d| !d.is_empty())
}

/// Collapse every run of ASCII whitespace (spaces, tabs, `\r`, `\n`) in `s` to a
/// single space and trim the ends. Restores the single-line description strings
/// schemars 0.8 produced from multi-line rustdoc, so consumers never see a
/// doc-comment's hard line wraps mid-sentence.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Recursively collapse whitespace in every `description` string anywhere in a
/// schema `Value` tree. Applied to the raw serialized schema embedded in the
/// skill snapshot, so its `description` keywords match the collapsed strings the
/// `explain` field-walk emits.
fn normalize_descriptions(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(d)) = map.get_mut("description") {
                *d = collapse_ws(d);
            }
            for v in map.values_mut() {
                normalize_descriptions(v);
            }
        }
        Value::Array(items) => {
            for v in items {
                normalize_descriptions(v);
            }
        }
        _ => {}
    }
}

/// JSON Schema draft-07 dialect URL. The standalone editor schemas
/// (`schemas/cfgd-*.schema.json`) and the embedded skill fallback schema are
/// both published as draft-07, so they share this stamp.
pub const DRAFT_07_DIALECT: &str = "https://json-schema.org/draft-07/schema#";

/// Downgrade a schemars 1.x schema `Value` (draft-2020-12 idiom) to the draft-07
/// idiom: stamp the draft-07 `$schema`, rename the root `$defs` object to
/// `definitions`, and rewrite every `#/$defs/...` `$ref` to `#/definitions/...`.
///
/// Shared by the standalone schema generator (`gen_schemas` bin, which then
/// overrides `$schema` with the per-file dialect) and [`KindEntry::json_schema`]
/// /[`KindEntry::pretty_schema`], so the embedded skill schema and the published
/// editor schemas stay on the same dialect and definition idiom.
pub fn migrate_to_draft_07(value: &mut Value) {
    if let Value::Object(root) = value {
        root.insert(
            "$schema".to_string(),
            Value::String(DRAFT_07_DIALECT.to_string()),
        );
        if let Some(defs) = root.remove("$defs") {
            root.insert("definitions".to_string(), defs);
        }
    }
    rewrite_def_refs(value);
}

/// Recursively rewrite every `$ref` string from the `#/$defs/` prefix to the
/// `#/definitions/` prefix (the draft-07 definition location).
fn rewrite_def_refs(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(r)) = map.get_mut("$ref")
                && let Some(rest) = r.strip_prefix("#/$defs/")
            {
                *r = format!("#/definitions/{rest}");
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

/// True when the (resolved) schema describes a JSON object with properties.
fn is_object(schema: &Value) -> bool {
    schema
        .as_object()
        .and_then(|o| o.get("properties"))
        .and_then(Value::as_object)
        .map(|props| !props.is_empty())
        .unwrap_or(false)
}

/// Map a (resolved) schema to cfgd's type description: `[]<inner>` for arrays,
/// `object` for objects/maps, otherwise the JSON instance type (`string`,
/// `integer`, `boolean`, …). Falls back to `object` when no type is declared
/// (e.g. enums, untyped maps).
fn type_description(
    schema: &Value,
    ctx: SchemaCtx,
    visited: &mut std::collections::BTreeSet<String>,
) -> String {
    if let Some(item) = array_item(schema) {
        let inner = array_inner_type(item, ctx, visited);
        return format!("[]{inner}");
    }
    if is_object(schema) {
        return "object".to_string();
    }
    match schema.as_object().and_then(|o| o.get("type")) {
        Some(Value::String(t)) => t.clone(),
        // A type union (`["string", "null"]`) takes the first non-null member,
        // matching how the 0.8 walk skipped the `null` instance type.
        Some(Value::Array(types)) => types
            .iter()
            .filter_map(Value::as_str)
            .find(|name| *name != "null")
            .map(str::to_string)
            .unwrap_or_else(|| "object".to_string()),
        _ => "object".to_string(),
    }
}

/// Type description of an array element, guarding the element `$ref` against a
/// cycle (a `Vec` whose element type `$ref`s back onto the descent path renders
/// as `object` rather than recursing).
fn array_inner_type(
    item: &Value,
    ctx: SchemaCtx,
    visited: &mut std::collections::BTreeSet<String>,
) -> String {
    let descent = RefDescent::enter(item, visited);
    let resolved = resolve_ref(item, ctx);
    let desc = if descent.safe() {
        type_description(&resolved, ctx, visited)
    } else {
        "object".to_string()
    };
    descent.leave(visited);
    desc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_lists_every_kind_local_and_crd() {
        let kinds: Vec<&str> = KIND_REGISTRY.iter().map(|e| e.kind).collect();
        // TeamConfig is intentionally absent: it is a Crossplane composite
        // resource with no Rust spec type to derive a schema from, so it cannot
        // carry a `schema_fn` like every other registry entry.
        for k in [
            "Module",
            "Profile",
            "ConfigSource",
            "Config",
            "MachineConfig",
            "ConfigPolicy",
            "ClusterConfigPolicy",
            "DriftAlert",
        ] {
            assert!(kinds.contains(&k), "missing {k}");
        }
    }

    #[test]
    fn field_tree_is_generated_from_schema() {
        let entry = KIND_REGISTRY
            .iter()
            .find(|e| e.kind == "Module" && !e.crd)
            .unwrap();
        assert!(entry.field_tree().iter().any(|f| f.name == "packages"));
    }

    #[test]
    fn crd_field_tree_comes_from_cfgd_crd_schemars() {
        let entry = KIND_REGISTRY
            .iter()
            .find(|e| e.kind == "ClusterConfigPolicy")
            .unwrap();
        assert!(
            !entry.field_tree().is_empty(),
            "CRD schema must resolve via cfgd-crd"
        );
    }

    #[test]
    fn local_and_crd_module_coexist() {
        let modules: Vec<&KindEntry> = KIND_REGISTRY
            .iter()
            .filter(|e| e.kind == "Module")
            .collect();
        assert_eq!(modules.len(), 2, "local + CRD Module both registered");
        assert!(modules.iter().any(|e| !e.crd), "local Module present");
        assert!(modules.iter().any(|e| e.crd), "CRD Module present");
    }

    #[test]
    fn array_fields_carry_slice_type_desc() {
        let entry = KIND_REGISTRY
            .iter()
            .find(|e| e.kind == "Module" && !e.crd)
            .unwrap();
        let packages = entry
            .field_tree()
            .into_iter()
            .find(|f| f.name == "packages")
            .unwrap();
        assert!(
            packages.type_desc.starts_with("[]"),
            "packages should be a slice type, got {}",
            packages.type_desc
        );
    }

    // A deliberately self-referential pair of types. `edge` and `target` are
    // bare (non-optional) `$ref`s — exactly the shape `resolve_ref` follows —
    // so the walk recurses Node -> Edge -> Node -> Edge ... Without a cycle
    // guard this overflows the stack and aborts the process.
    #[derive(schemars::JsonSchema)]
    #[allow(dead_code)]
    struct Node {
        name: String,
        edge: Edge,
    }

    #[derive(schemars::JsonSchema)]
    #[allow(dead_code)]
    struct Edge {
        target: Box<Node>,
    }

    #[test]
    fn self_referential_schema_terminates_with_bounded_tree() {
        let schema = schema_for!(Node);
        // The contract under test is termination: this returns instead of
        // overflowing the stack on the recursive `edge`/`target` refs.
        let tree = field_tree_from_schema(&schema);

        let names: Vec<&str> = tree.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"name"), "expected `name`, got {names:?}");
        assert!(names.contains(&"edge"), "expected `edge`, got {names:?}");

        // The descent unrolls Node -> edge(Edge) -> target(Node) -> edge(Edge),
        // and the second Edge re-entry is cut: that inner `edge` renders as a
        // leaf rather than recursing forever. A tree, not a collapsed DAG — the
        // first `edge` and `target` still expand their one level.
        let edge = tree
            .iter()
            .find(|f| f.name == "edge")
            .expect("edge present");
        assert_eq!(edge.type_desc, "object");

        let target = edge
            .children
            .iter()
            .find(|f| f.name == "target")
            .expect("edge.target present");
        assert_eq!(target.type_desc, "object");

        // `target` re-enters Node and expands one level (its own `edge`/`name`),
        // where the recursive `edge` is finally cut to a leaf.
        let inner_edge = target
            .children
            .iter()
            .find(|f| f.name == "edge")
            .expect("target.edge present");
        assert_eq!(inner_edge.type_desc, "object");
        assert!(
            inner_edge.children.is_empty(),
            "recursive edge must be cut to a leaf, got {:?}",
            inner_edge.children
        );
    }

    // A type whose element type is itself — the array-recursion path.
    // `kids: Vec<TreeNode>` makes `array_element_fields` descend TreeNode ->
    // kids[](TreeNode) -> kids[](TreeNode) ... so without the `RefDescent`
    // guard on the element `$ref` the walk overflows the stack.
    #[derive(schemars::JsonSchema)]
    #[allow(dead_code)]
    struct TreeNode {
        name: String,
        kids: Vec<TreeNode>,
    }

    #[test]
    fn self_referential_array_terminates_with_bounded_tree() {
        let schema = schema_for!(TreeNode);
        // Termination: returns instead of overflowing on the recursive `kids`
        // element `$ref`.
        let tree = field_tree_from_schema(&schema);

        let names: Vec<&str> = tree.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"name"), "expected `name`, got {names:?}");
        assert!(names.contains(&"kids"), "expected `kids`, got {names:?}");

        // `kids` is an array of TreeNode; its element fields expand one level
        // (the element's own `name`/`kids`), where the recursive `kids` is cut.
        let kids = tree
            .iter()
            .find(|f| f.name == "kids")
            .expect("kids present");
        assert!(
            kids.type_desc.starts_with("[]"),
            "kids should be a slice type, got {}",
            kids.type_desc
        );

        let inner_kids = kids
            .children
            .iter()
            .find(|f| f.name == "kids")
            .expect("kids[].kids present");
        // The guard fired: the recursive element is cut to a leaf, proving the
        // walk did not descend infinitely.
        assert!(
            inner_kids.children.is_empty(),
            "recursive kids element must be cut to a leaf, got {:?}",
            inner_kids.children
        );
    }

    // A type with an `Option<Box<Self>>` field — the option-wrapped self-ref
    // path. `schemars` renders `Option<RefType>` as an `allOf`/`anyOf` wrapper
    // around the `$ref`, which `unwrap_single_subschema` peels before
    // `field_node` follows the ref. Without the guard on that unwrapped ref the
    // walk recurses ListNode -> next(ListNode) -> next(ListNode) ... forever.
    #[derive(schemars::JsonSchema)]
    #[allow(dead_code)]
    struct ListNode {
        value: String,
        next: Option<Box<ListNode>>,
    }

    #[test]
    fn option_wrapped_self_ref_terminates_with_bounded_tree() {
        let schema = schema_for!(ListNode);
        // Termination: returns instead of overflowing on the recursive,
        // option-wrapped `next` ref.
        let tree = field_tree_from_schema(&schema);

        let names: Vec<&str> = tree.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"value"), "expected `value`, got {names:?}");
        assert!(names.contains(&"next"), "expected `next`, got {names:?}");

        // `next` unwraps to the ListNode object and expands one level (its own
        // `value`/`next`), where the recursive `next` is finally cut to a leaf.
        let next = tree
            .iter()
            .find(|f| f.name == "next")
            .expect("next present");
        assert_eq!(next.type_desc, "object");

        let inner_next = next
            .children
            .iter()
            .find(|f| f.name == "next")
            .expect("next.next present");
        // The guard fired on the unwrapped ref: the recursive `next` is a leaf.
        assert!(
            inner_next.children.is_empty(),
            "recursive next must be cut to a leaf, got {:?}",
            inner_next.children
        );
    }
}
