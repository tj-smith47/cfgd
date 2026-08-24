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

use std::collections::HashMap;

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
    /// The named `$defs` entry this field's schema resolved through — the
    /// field's own `$ref` target, or its array element's (`files` →
    /// `ModuleFileEntry`). Empty for an inline anonymous schema, for a scalar,
    /// and for a bare root self-reference.
    ///
    /// DISPLAY only: [`FieldNode::displayed_type`] substitutes it for the shape
    /// word so a reader sees the type they can look up, while `type_desc`
    /// stays the shape vocabulary (`object`, `[]object`) every existing
    /// consumer reads.
    pub type_name: String,
    /// The accepted values of a unit-variant enum (`Copy`, `Symlink`, …), in
    /// declared order. Empty for every other field.
    pub enum_values: Vec<String>,
    /// Nested fields, for object-typed fields (including an array field's
    /// object-shaped element, e.g. `packages[].name`).
    pub children: Vec<FieldNode>,
    /// Every accepted shape, for a field whose schema is a genuine multi-shape
    /// union (an untagged Rust enum like `ScriptEntry`, whose variants render
    /// to *different* type descriptions — `string` and `object` are both
    /// legal). Each entry's `name`/`type_desc` is the shape's own type label
    /// (e.g. `"string"`, `"object"`) and its `children` are that shape's own
    /// fields when it is object-shaped. Empty for every other field, including
    /// a union whose members all collapse to one instance type (a unit-variant
    /// enum, an `Option<T>`) — those already carry that single type in
    /// `type_desc` and need no shape breakdown.
    pub variants: Vec<FieldNode>,
}

impl FieldNode {
    /// The type as a reader sees it: the named `$defs` type when the field
    /// resolved through one (`[]ModuleFileEntry`, `ScriptSpec`), else the
    /// shape word in `type_desc`.
    ///
    /// The `[]` prefix is carried over from `type_desc` because a name is
    /// recorded for the ELEMENT of an array field; no named definition in the
    /// registry is itself array-shaped, so the two can never both contribute a
    /// prefix.
    pub fn displayed_type(&self) -> String {
        match (self.type_name.as_str(), self.type_desc.strip_prefix("[]")) {
            ("", _) => self.type_desc.clone(),
            (name, Some(_)) => format!("[]{name}"),
            (name, None) => name.to_string(),
        }
    }
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
    /// Where this kind is documented in the repo's `docs/` tree, as a path
    /// plus heading anchor (`docs/spec/module.md#fields`). Shown by `explain`
    /// so a reader who wants prose has one hop to it.
    pub docs: &'static str,
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

/// Per-kind memo of [`KindEntry::canonical_schema_value`], keyed by the pair
/// that identifies a registry entry: the CRD `Module` shares its `kind` string
/// with the local one.
type CanonicalSchemaCache = std::sync::Mutex<HashMap<(&'static str, bool), std::sync::Arc<Value>>>;

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
        serde_json::to_string(self.canonical_schema_value().as_ref()).unwrap_or_default()
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
        serde_json::to_string_pretty(self.canonical_schema_value().as_ref()).unwrap_or_default()
    }

    /// Schema as a `Value` in cfgd's canonical published form: draft-07 dialect
    /// and definition idiom with whitespace-collapsed descriptions. The single
    /// transform behind both [`KindEntry::json_schema`] and
    /// [`KindEntry::pretty_schema`].
    ///
    /// Derived at most once per kind per process. `schema_fn` re-runs
    /// `schemars::schema_for!` and the two normalizers then walk the whole
    /// document, so a caller asking for both the compact and the pretty form of
    /// one kind paid for that walk twice. Only the serialization is still per
    /// call, which is what keeps the compact caller from paying to pretty-print.
    /// The `Value` is a function of Rust types compiled into this binary, so it
    /// has no invalidation counterpart.
    fn canonical_schema_value(&self) -> std::sync::Arc<Value> {
        static CANONICAL: std::sync::OnceLock<CanonicalSchemaCache> = std::sync::OnceLock::new();
        let cache = CANONICAL.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
        // A poisoned lock means another thread panicked mid-derivation; the map
        // holds only pure derived values, so reusing it is safe and refusing to
        // would turn a schema panic into a second, unrelated one.
        if let Ok(map) = cache.lock()
            && let Some(hit) = map.get(&(self.kind, self.crd))
        {
            return hit.clone();
        }
        let mut value =
            serde_json::to_value((self.schema_fn)()).unwrap_or(Value::Object(Default::default()));
        normalize_descriptions(&mut value);
        migrate_to_draft_07(&mut value);
        let value = std::sync::Arc::new(value);
        if let Ok(mut map) = cache.lock() {
            map.insert((self.kind, self.crd), value.clone());
        }
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
        docs: "docs/spec/module.md#fields",
        schema_fn: || schema_for!(crate::config::ModuleSpec),
        validate_fn: validate_local::<crate::config::ModuleDocument>,
    },
    KindEntry {
        kind: "Profile",
        api_version: crate::API_VERSION,
        location: "profiles/<name>/profile.yaml",
        description: "A composable layer of modules, packages, files, and settings.",
        crd: false,
        docs: "docs/spec/profile.md#fields",
        schema_fn: || schema_for!(crate::config::ProfileSpec),
        validate_fn: validate_local::<crate::config::ProfileDocument>,
    },
    KindEntry {
        kind: "ConfigSource",
        api_version: crate::API_VERSION,
        location: "cfgd-source.yaml",
        description: "A published source of modules and profiles for multi-source config.",
        crd: false,
        docs: "docs/sources.md#configsource-manifest",
        schema_fn: || schema_for!(crate::config::ConfigSourceSpec),
        validate_fn: validate_local::<crate::config::ConfigSourceDocument>,
    },
    KindEntry {
        kind: "Config",
        api_version: crate::API_VERSION,
        location: "cfgd.yaml",
        description: "The root cfgd configuration: active profile, sources, daemon, theme.",
        crd: false,
        docs: "docs/spec/config.md#fields",
        schema_fn: || schema_for!(crate::config::CfgdConfig),
        validate_fn: validate_local::<crate::config::CfgdConfig>,
    },
    #[cfg(feature = "crd")]
    KindEntry {
        kind: "MachineConfig",
        api_version: crate::API_VERSION,
        location: "MachineConfig CRD",
        description: "Per-machine desired state reconciled by the cfgd operator.",
        crd: true,
        docs: "docs/spec/machineconfig.md#fields",
        schema_fn: || schema_for!(cfgd_crd::MachineConfigSpec),
        validate_fn: validate_crd_spec::<cfgd_crd::MachineConfigSpec>,
    },
    #[cfg(feature = "crd")]
    KindEntry {
        kind: "ConfigPolicy",
        api_version: crate::API_VERSION,
        location: "ConfigPolicy CRD",
        description: "Namespace-scoped policy of required modules, packages, and settings.",
        crd: true,
        docs: "docs/spec/configpolicy.md#fields",
        schema_fn: || schema_for!(cfgd_crd::ConfigPolicySpec),
        validate_fn: validate_crd_spec::<cfgd_crd::ConfigPolicySpec>,
    },
    #[cfg(feature = "crd")]
    KindEntry {
        kind: "ClusterConfigPolicy",
        api_version: crate::API_VERSION,
        location: "ClusterConfigPolicy CRD",
        description: "Cluster-scoped policy fanned out across selected namespaces.",
        crd: true,
        docs: "docs/spec/clusterconfigpolicy.md#fields",
        schema_fn: || schema_for!(cfgd_crd::ClusterConfigPolicySpec),
        validate_fn: validate_crd_spec::<cfgd_crd::ClusterConfigPolicySpec>,
    },
    #[cfg(feature = "crd")]
    KindEntry {
        kind: "DriftAlert",
        api_version: crate::API_VERSION,
        location: "DriftAlert CRD",
        description: "A recorded drift event between desired and observed machine state.",
        crd: true,
        docs: "docs/spec/driftalert.md#fields",
        schema_fn: || schema_for!(cfgd_crd::DriftAlertSpec),
        validate_fn: validate_crd_spec::<cfgd_crd::DriftAlertSpec>,
    },
    #[cfg(feature = "crd")]
    KindEntry {
        kind: "Module",
        api_version: crate::API_VERSION,
        location: "Module CRD",
        description: "Cluster-side Module CRD: an OCI-packaged module injected via CSI.",
        crd: true,
        docs: "docs/operator.md#pod-module-injection",
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
    // stay drillable (e.g. `packages[].name`). `variants` covers the third
    // shape: a field (or an array's element) whose schema is itself a
    // multi-shape union — neither an object nor unwrappable to one, so
    // `children` stays empty and the shapes live in `variants` instead. A
    // `$ref` re-entry (cycle) stops here, emitting the field as a leaf with
    // neither.
    let (children, variants) = if !descent.safe() {
        (Vec::new(), Vec::new())
    } else if is_object(&resolved) {
        (object_fields(&resolved, ctx, visited), Vec::new())
    } else if array_item(&resolved).is_some() {
        (
            array_element_fields(&resolved, ctx, visited),
            array_element_variants(&resolved, ctx, visited),
        )
    } else {
        (Vec::new(), union_variants(&resolved, ctx, visited))
    };
    descent.leave(visited);
    FieldNode {
        name: name.to_string(),
        type_desc,
        type_name: named_type(&unwrapped, &resolved).unwrap_or_default(),
        enum_values: enum_values(&resolved, ctx),
        required,
        description,
        children,
        variants,
    }
}

/// The `$defs` name a schema resolves through: its own `$ref` target, or — for
/// an array — its element's. `None` for an inline schema and for a bare root
/// self-reference, neither of which names a type a reader could look up.
fn named_type(unwrapped: &Value, resolved: &Value) -> Option<String> {
    fn named_ref(schema: &Value) -> Option<String> {
        ref_name(schema).filter(|name| name != ROOT_REF)
    }
    named_ref(unwrapped).or_else(|| named_ref(&unwrap_single_subschema(array_item(resolved)?)))
}

/// The accepted values of a unit-variant enum, in declared order: schemars
/// emits either a `oneOf` of `const` members (when the variants carry rustdoc)
/// or a plain `enum` array (when they do not), and both spellings occur across
/// the registry. An array of such an enum answers with its element's values, so
/// `[]DriftSeverity` discloses them the same way a bare field does. Empty for
/// anything else — including a union whose members are not all consts, which is
/// a multi-shape union rather than a value list.
fn enum_values(schema: &Value, ctx: SchemaCtx) -> Vec<String> {
    let direct = enum_values_here(schema);
    if !direct.is_empty() {
        return direct;
    }
    match array_item(schema) {
        Some(item) => enum_values_here(&resolve_ref(&unwrap_single_subschema(item), ctx)),
        None => Vec::new(),
    }
}

/// [`enum_values`] for one schema, without unwrapping an array element.
fn enum_values_here(schema: &Value) -> Vec<String> {
    let Some(obj) = schema.as_object() else {
        return Vec::new();
    };
    if let Some(one) = obj.get("const").and_then(Value::as_str) {
        return vec![one.to_string()];
    }
    if let Some(values) = obj.get("enum").and_then(Value::as_array) {
        return values
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
    }
    let Some(members) = obj
        .get("oneOf")
        .or_else(|| obj.get("anyOf"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    // A member contributes either one `const` (a documented variant) or a
    // whole `enum` array (the undocumented ones, which schemars collapses into
    // a single member) — `ScriptShell` carries both at once. A member that
    // contributes neither makes the union a multi-shape one rather than a
    // value list, and the whole answer is empty.
    let mut values = Vec::new();
    for member in members.iter().filter(|m| !is_null_schema(m)) {
        let member = enum_values_here(member);
        if member.is_empty() {
            return Vec::new();
        }
        values.extend(member);
    }
    values
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

/// For an array (resolved) schema whose element type is a genuine multi-shape
/// union (e.g. `Vec<ScriptEntry>`, each entry a `string` or a `{ run, … }`
/// object), return one [`FieldNode`] per accepted element shape. Mirrors
/// [`array_element_fields`]'s walk to the (unwrapped, `$ref`-resolved) element
/// schema, but hands it to [`union_variants`] instead of [`object_fields`].
/// Returns an empty vec for an array of scalars or of plain objects — either
/// already carries its shape in `type_desc`/`children` and has no second shape
/// to disclose.
fn array_element_variants(
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
    let variants = if descent.safe() {
        union_variants(&resolved, ctx, visited)
    } else {
        Vec::new()
    };
    descent.leave(visited);
    variants
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
/// `object` for objects/maps, the JSON instance type (`string`, `integer`,
/// `boolean`, …) for a scalar, or a parenthesised `(a | b)` join of every
/// accepted shape for a genuine multi-shape union (an untagged enum whose
/// variants render to different types, e.g. `ScriptEntry`'s `(string |
/// object)`) — see [`union_variants`] for the matching field-tree expansion.
/// Falls back to `object` only when no type can be determined at all (an
/// untyped map, or a union with a cyclic member).
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
    if let Some(member) = union_member_type(schema, ctx, visited) {
        return member;
    }
    if let Some(joined) = union_type_join(schema, ctx, visited) {
        return format!("({joined})");
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

/// Type description of a `oneOf`/`anyOf` whose members all describe the same
/// instance type.
///
/// schemars renders a unit-variant enum as a `oneOf` of `const` members and an
/// `Option<T>` as an `anyOf` of `T` and `null`, neither of which carries a
/// top-level `type` — without this every such field renders as `object`, so
/// `cfgd explain` typed the same `FileStrategy` two different ways depending on
/// whether the field happened to have an inline schema.
///
/// `null` members are skipped: they encode optionality, not a type. A union of
/// genuinely different types has no single answer and yields `None`, leaving
/// the caller's `object` fallback in place.
fn union_member_type(
    schema: &Value,
    ctx: SchemaCtx,
    visited: &mut std::collections::BTreeSet<String>,
) -> Option<String> {
    let obj = schema.as_object()?;
    let members = obj
        .get("oneOf")
        .or_else(|| obj.get("anyOf"))
        .and_then(Value::as_array)?;
    let mut found: Option<String> = None;
    for member in members {
        if is_null_schema(member) {
            continue;
        }
        let descent = RefDescent::enter(member, visited);
        // A member that `$ref`s back onto the descent path cannot be described
        // without recursing forever; treat the whole union as undecidable.
        if !descent.safe() {
            descent.leave(visited);
            return None;
        }
        let resolved = resolve_ref(member, ctx);
        let desc = type_description(&resolved, ctx, visited);
        descent.leave(visited);
        match &found {
            None => found = Some(desc),
            Some(prev) if *prev == desc => {}
            Some(_) => return None,
        }
    }
    found
}

/// Join every distinct accepted shape's type description with `" | "`, for the
/// [`type_description`] fallback tier reached when [`union_member_type`] cannot
/// collapse the union to one type. `None` when the union collapses to a single
/// type after dedup (redundant with `union_member_type`'s tier) or a member
/// sits on a cyclic `$ref` — either way the caller's final `object` fallback
/// applies instead.
fn union_type_join(
    schema: &Value,
    ctx: SchemaCtx,
    visited: &mut std::collections::BTreeSet<String>,
) -> Option<String> {
    let obj = schema.as_object()?;
    let members = obj
        .get("oneOf")
        .or_else(|| obj.get("anyOf"))
        .and_then(Value::as_array)?;
    let mut parts: Vec<String> = Vec::new();
    for member in members {
        if is_null_schema(member) {
            continue;
        }
        let descent = RefDescent::enter(member, visited);
        if !descent.safe() {
            descent.leave(visited);
            return None;
        }
        let resolved = resolve_ref(member, ctx);
        let desc = type_description(&resolved, ctx, visited);
        descent.leave(visited);
        if !parts.contains(&desc) {
            parts.push(desc);
        }
    }
    (parts.len() > 1).then(|| parts.join(" | "))
}

/// Field-tree expansion of a genuine multi-shape union: one [`FieldNode`] per
/// distinct accepted shape, named by its own [`type_description`] (schemars
/// drops Rust variant names for `#[serde(untagged)]` enums, so the type string
/// is the only stable label available — e.g. `ScriptEntry` yields a `string`
/// variant and an `object` variant carrying its `run`/`timeout`/… fields).
/// Returns an empty vec when the union already collapses to one type via
/// [`union_member_type`] (nothing to break down) or the schema is not a union.
fn union_variants(
    schema: &Value,
    ctx: SchemaCtx,
    visited: &mut std::collections::BTreeSet<String>,
) -> Vec<FieldNode> {
    if union_member_type(schema, ctx, visited).is_some() {
        return Vec::new();
    }
    let Some(members) = schema
        .as_object()
        .and_then(|o| o.get("oneOf").or_else(|| o.get("anyOf")))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut variants = Vec::new();
    for member in members {
        if is_null_schema(member) {
            continue;
        }
        let descent = RefDescent::enter(member, visited);
        if !descent.safe() {
            descent.leave(visited);
            continue;
        }
        let resolved = resolve_ref(member, ctx);
        let type_desc = type_description(&resolved, ctx, visited);
        if seen.insert(type_desc.clone()) {
            let description = schema_description(member)
                .or_else(|| schema_description(&resolved))
                .unwrap_or_default();
            let children = if is_object(&resolved) {
                object_fields(&resolved, ctx, visited)
            } else {
                Vec::new()
            };
            variants.push(FieldNode {
                name: type_desc.clone(),
                type_desc,
                // A variant row is a SHAPE the field accepts, not a field of
                // its own: the union's name belongs to the field above it,
                // which already renders it.
                type_name: String::new(),
                enum_values: Vec::new(),
                required: false,
                description,
                children,
                variants: Vec::new(),
            });
        }
        descent.leave(visited);
    }
    variants
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

    /// schemars renders a unit-variant enum as a `oneOf` of `const` members
    /// with no top-level `type`. Reporting those as `object` told the operator
    /// to write a mapping where a bare string belongs.
    #[test]
    fn unit_variant_enum_fields_report_their_instance_type() {
        let entry = KIND_REGISTRY
            .iter()
            .find(|e| e.kind == "Profile" && !e.crd)
            .expect("Profile kind is registered");
        let env_scope = entry
            .field_tree()
            .into_iter()
            .find(|f| f.name == "envScope")
            .expect("envScope is a Profile field");
        assert_eq!(env_scope.type_desc, "string");
    }

    /// Type description of the named property of an inline JSON schema.
    fn type_desc_of(schema: serde_json::Value, field: &str) -> String {
        let schema: Schema = serde_json::from_value(schema).expect("schema parses");
        field_tree_from_schema(&schema)
            .into_iter()
            .find(|f| f.name == field)
            .unwrap_or_else(|| panic!("{field} present"))
            .type_desc
    }

    /// `Option<T>` is an `anyOf` of `T` and `null`. The `null` member encodes
    /// optionality, not a type, so it must not defeat the union.
    #[test]
    fn optional_scalar_fields_report_the_non_null_member_type() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "maybe": { "anyOf": [{ "type": "string" }, { "type": "null" }] },
                "count": { "anyOf": [{ "type": "integer" }, { "type": "null" }] },
            }
        });
        assert_eq!(type_desc_of(schema.clone(), "maybe"), "string");
        assert_eq!(type_desc_of(schema, "count"), "integer");
    }

    /// A union of genuinely different types has no single answer, so the walk
    /// joins every distinct member type instead of picking one at random or
    /// collapsing to the opaque `object` fallback.
    #[test]
    fn a_mixed_type_union_joins_member_types() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "either": { "anyOf": [{ "type": "string" }, { "type": "integer" }] }
            }
        });
        assert_eq!(type_desc_of(schema, "either"), "(string | integer)");
    }

    /// The field's `variants` carry one [`FieldNode`] per distinct accepted
    /// shape, labeled by its own type description, with the object variant's
    /// own fields expanded as `children` — the `ScriptEntry`-shaped case this
    /// gap exists for (`string` or `{ run, timeout, … }`).
    #[test]
    fn a_mixed_type_union_expands_variants() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "hook": {
                    "anyOf": [
                        { "type": "string" },
                        {
                            "type": "object",
                            "properties": { "run": { "type": "string" } },
                            "required": ["run"]
                        }
                    ]
                }
            }
        });
        let schema: Schema = serde_json::from_value(schema).expect("schema parses");
        let tree = field_tree_from_schema(&schema);
        let hook = tree.iter().find(|f| f.name == "hook").expect("hook field");
        let variant_types: Vec<&str> = hook.variants.iter().map(|v| v.type_desc.as_str()).collect();
        assert_eq!(variant_types, vec!["string", "object"]);
        let object_variant = hook
            .variants
            .iter()
            .find(|v| v.type_desc == "object")
            .expect("object variant");
        let child_names: Vec<&str> = object_variant
            .children
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(child_names, vec!["run"]);
    }

    /// An `Option<SomeObject>` union must still resolve through the member
    /// `$ref` to `object` — the union handling may not shortcut a `$ref`.
    #[test]
    fn optional_object_fields_stay_object() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "nested": { "anyOf": [{ "$ref": "#/$defs/Inner" }, { "type": "null" }] }
            },
            "$defs": {
                "Inner": { "type": "object", "properties": { "a": { "type": "string" } } }
            }
        });
        assert_eq!(type_desc_of(schema, "nested"), "object");
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

    #[test]
    fn a_kind_derives_its_canonical_schema_once_per_process() {
        // Deriving re-runs `schemars::schema_for!` and then walks the whole
        // document twice to normalize it. A caller asking for both the compact
        // and the pretty form of one kind must not pay for that twice, so the
        // two calls hand back the SAME allocation rather than equal ones.
        let entry = KIND_REGISTRY
            .iter()
            .find(|e| e.kind == "Profile")
            .expect("Profile is a registered kind");

        let first = entry.canonical_schema_value();
        let second = entry.canonical_schema_value();
        assert!(std::sync::Arc::ptr_eq(&first, &second));

        // The memo is keyed by kind AND crd flag, so a different kind is a
        // different derivation rather than the first one handed out again.
        let other = KIND_REGISTRY
            .iter()
            .find(|e| e.kind == "Config")
            .expect("Config is a registered kind");
        assert!(!std::sync::Arc::ptr_eq(
            &first,
            &other.canonical_schema_value()
        ));
    }

    /// [`FieldNode::displayed_type`] carries the `[]` prefix over from
    /// `type_desc` and takes the rest of the word from `type_name`, on the
    /// premise that a named definition is never itself array-shaped. A def that
    /// were would render `[][]Thing` or swallow the prefix, so the premise is
    /// pinned here rather than left to the first reader who trips it.
    #[test]
    fn no_named_definition_in_the_registry_is_itself_array_shaped() {
        for entry in KIND_REGISTRY {
            let schema = entry.canonical_schema_value();
            let Some(defs) = schema
                .get("$defs")
                .or_else(|| schema.get("definitions"))
                .and_then(Value::as_object)
            else {
                continue;
            };
            for (name, def) in defs {
                assert_ne!(
                    def.get("type").and_then(Value::as_str),
                    Some("array"),
                    "{}'s $defs entry {name} is array-shaped, which breaks displayed_type's [] prefix rule",
                    entry.kind
                );
            }
        }
    }

    fn module_field(name: &str) -> FieldNode {
        KIND_REGISTRY
            .iter()
            .find(|e| e.kind == "Module" && !e.crd)
            .expect("Module is a registered kind")
            .field_tree()
            .into_iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("Module has no {name} field"))
    }

    #[test]
    fn a_field_resolving_through_a_named_definition_displays_that_name() {
        let files = module_field("files");
        // The shape word is the wire value and never moves.
        assert_eq!(files.type_desc, "[]object");
        assert_eq!(files.type_name, "ModuleFileEntry");
        assert_eq!(files.displayed_type(), "[]ModuleFileEntry");

        let scripts = module_field("scripts");
        assert_eq!(scripts.type_desc, "object");
        assert_eq!(scripts.displayed_type(), "ScriptSpec");

        // An inline anonymous map has no definition to name, and displays the
        // shape word unchanged.
        let system = module_field("system");
        assert_eq!(system.type_name, "");
        assert_eq!(system.displayed_type(), "object");
    }

    #[test]
    fn a_union_mixing_a_const_with_an_enum_array_lists_every_accepted_value() {
        // `ScriptShell` is a oneOf whose documented variants each carry one
        // `const` while schemars collapses the undocumented ones into a single
        // member holding a whole `enum` array. Both halves have to contribute
        // or the field renders no accepted values at all.
        let shell = KIND_REGISTRY
            .iter()
            .find(|e| e.kind == "Profile" && !e.crd)
            .expect("Profile is a registered kind")
            .field_tree()
            .into_iter()
            .find(|f| f.name == "scripts")
            .and_then(|f| f.children.into_iter().find(|c| c.name == "preApply"))
            .and_then(|f| f.variants.into_iter().find(|v| v.type_desc == "object"))
            .and_then(|v| v.children.into_iter().find(|c| c.name == "shell"))
            .expect("Profile scripts.preApply object variant carries shell");
        assert_eq!(
            shell.enum_values,
            vec!["sh", "bash", "zsh", "pwsh", "cmd", "auto"]
        );
    }

    #[test]
    fn every_registered_kind_points_at_a_docs_anchor() {
        for entry in KIND_REGISTRY {
            assert!(
                entry.docs.starts_with("docs/") && entry.docs.contains('#'),
                "{} has no docs anchor: {:?}",
                entry.kind,
                entry.docs
            );
        }
    }
}
