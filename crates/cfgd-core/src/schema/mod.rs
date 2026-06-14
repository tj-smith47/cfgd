//! Unified resource-kind registry.
//!
//! [`KIND_REGISTRY`] is the single source of truth for every cfgd resource kind
//! — both the local YAML document kinds (`Module`, `Profile`, `ConfigSource`,
//! `Config`) and the cluster-side CRD kinds delivered by the [`cfgd_crd`] crate
//! (`MachineConfig`, `ConfigPolicy`, `ClusterConfigPolicy`, `DriftAlert`, and the
//! CRD `Module`). Each [`KindEntry`] carries a `schema_fn` that returns the
//! kind's `schemars`-derived [`RootSchema`], so `explain`, `validate`, and the
//! skill installer all read schemas from one place and can never drift apart.
//!
//! The CRD half of the registry is compiled behind the default-on `crd` Cargo
//! feature. Consumers that never touch Kubernetes resources (notably the CSI
//! node plugin) depend on `cfgd-core` with `default-features = false` to keep
//! the heavy `kube`/`k8s-openapi` stack out of their binary.

pub mod snapshot;

use schemars::schema::{RootSchema, Schema, SchemaObject, SingleOrVec};
use schemars::schema_for;

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
    /// Returns the kind's `schemars`-derived root schema.
    pub schema_fn: fn() -> RootSchema,
}

impl KindEntry {
    /// Resolve this kind's schema into a [`FieldNode`] tree (top-level `spec`
    /// fields, with nested object fields recursed).
    pub fn field_tree(&self) -> Vec<FieldNode> {
        field_tree_from_schema(&(self.schema_fn)())
    }

    /// Serialize this kind's schema as a JSON string. Empty on serialization
    /// failure (schemars schemas are infallibly serializable, so this never
    /// observably empties in practice).
    pub fn json_schema(&self) -> String {
        serde_json::to_string(&(self.schema_fn)()).unwrap_or_default()
    }
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
    },
    KindEntry {
        kind: "Profile",
        api_version: crate::API_VERSION,
        location: "profiles/<name>.yaml",
        description: "A composable layer of modules, packages, files, and settings.",
        crd: false,
        schema_fn: || schema_for!(crate::config::ProfileSpec),
    },
    KindEntry {
        kind: "ConfigSource",
        api_version: crate::API_VERSION,
        location: "cfgd-source.yaml",
        description: "A published source of modules and profiles for multi-source config.",
        crd: false,
        schema_fn: || schema_for!(crate::config::ConfigSourceSpec),
    },
    KindEntry {
        kind: "Config",
        api_version: crate::API_VERSION,
        location: "cfgd.yaml",
        description: "The root cfgd configuration: active profile, sources, daemon, theme.",
        crd: false,
        schema_fn: || schema_for!(crate::config::CfgdConfig),
    },
    #[cfg(feature = "crd")]
    KindEntry {
        kind: "MachineConfig",
        api_version: crate::API_VERSION,
        location: "MachineConfig CRD (cfgd.io/v1alpha1)",
        description: "Per-machine desired state reconciled by the cfgd operator.",
        crd: true,
        schema_fn: || schema_for!(cfgd_crd::MachineConfigSpec),
    },
    #[cfg(feature = "crd")]
    KindEntry {
        kind: "ConfigPolicy",
        api_version: crate::API_VERSION,
        location: "ConfigPolicy CRD (cfgd.io/v1alpha1)",
        description: "Namespace-scoped policy of required modules, packages, and settings.",
        crd: true,
        schema_fn: || schema_for!(cfgd_crd::ConfigPolicySpec),
    },
    #[cfg(feature = "crd")]
    KindEntry {
        kind: "ClusterConfigPolicy",
        api_version: crate::API_VERSION,
        location: "ClusterConfigPolicy CRD (cfgd.io/v1alpha1)",
        description: "Cluster-scoped policy fanned out across selected namespaces.",
        crd: true,
        schema_fn: || schema_for!(cfgd_crd::ClusterConfigPolicySpec),
    },
    #[cfg(feature = "crd")]
    KindEntry {
        kind: "DriftAlert",
        api_version: crate::API_VERSION,
        location: "DriftAlert CRD (cfgd.io/v1alpha1)",
        description: "A recorded drift event between desired and observed machine state.",
        crd: true,
        schema_fn: || schema_for!(cfgd_crd::DriftAlertSpec),
    },
    #[cfg(feature = "crd")]
    KindEntry {
        kind: "Module",
        api_version: crate::API_VERSION,
        location: "Module CRD (cfgd.io/v1alpha1)",
        description: "Cluster-side Module CRD: an OCI-packaged module injected via CSI.",
        crd: true,
        schema_fn: || schema_for!(cfgd_crd::ModuleSpec),
    },
];

/// Walk a `schemars` [`RootSchema`] into a [`FieldNode`] tree.
///
/// Reads the root object's `properties` (skipping the KRM envelope keys
/// `apiVersion`/`kind`/`metadata`), descending into the `spec` object so the
/// tree presents authoring fields directly. `$ref`s are resolved against the
/// schema's `definitions`; nested object fields recurse; array element types
/// are unwrapped to a `[]<inner>` type description. Required-ness and
/// descriptions come from the schema. Pure — no I/O.
pub fn field_tree_from_schema(root: &RootSchema) -> Vec<FieldNode> {
    let mut visited = std::collections::BTreeSet::new();
    let top = object_properties(&root.schema);
    // KRM document schemas (Config) wrap authoring fields under `spec`; CRD and
    // bare-spec schemas already start at the spec object. Descend into `spec`
    // when present so every kind presents its authoring fields uniformly.
    if let Some((_, spec_schema)) = top.iter().find(|(name, _)| name.as_str() == "spec") {
        let descent = RefDescent::enter(spec_schema, &mut visited);
        let resolved = resolve_ref(spec_schema, root);
        let props = object_properties(&resolved);
        let fields = fields_from_properties(&props, &required_set(&resolved), root, &mut visited);
        descent.leave(&mut visited);
        return fields;
    }
    fields_from_properties(&top, &required_set(&root.schema), root, &mut visited)
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
    fn enter(schema: &Schema, visited: &mut std::collections::BTreeSet<String>) -> Self {
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

/// The definition name a schema `$ref`s (`#/definitions/<Name>` → `<Name>`),
/// or `None` for an inline schema carrying no `$ref`.
fn ref_name(schema: &Schema) -> Option<String> {
    let Schema::Object(obj) = schema else {
        return None;
    };
    obj.reference
        .as_ref()
        .and_then(|r| r.strip_prefix("#/definitions/"))
        .map(str::to_string)
}

/// Resolve a `$ref` (`#/definitions/<Name>`) against the root's `definitions`,
/// returning the referenced [`SchemaObject`]. Returns the input unchanged when
/// it carries no `$ref` or the target is missing (graceful, no panic).
fn resolve_ref(schema: &Schema, root: &RootSchema) -> SchemaObject {
    let Schema::Object(obj) = schema else {
        return SchemaObject::default();
    };
    if let Some(reference) = &obj.reference
        && let Some(name) = reference.strip_prefix("#/definitions/")
        && let Some(Schema::Object(target)) = root.definitions.get(name)
    {
        return target.clone();
    }
    obj.clone()
}

/// Extract the `(name, schema)` pairs from an object schema's `properties`.
fn object_properties(schema: &SchemaObject) -> Vec<(String, Schema)> {
    schema
        .object
        .as_ref()
        .map(|o| {
            o.properties
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// The set of required field names declared on an object schema.
fn required_set(schema: &SchemaObject) -> std::collections::BTreeSet<String> {
    schema
        .object
        .as_ref()
        .map(|o| o.required.iter().cloned().collect())
        .unwrap_or_default()
}

/// Build [`FieldNode`]s for every property of a (already `$ref`-resolved)
/// object schema. `visited` carries the `$ref` names on the current descent
/// path for cycle protection.
fn object_fields(
    schema: &SchemaObject,
    root: &RootSchema,
    visited: &mut std::collections::BTreeSet<String>,
) -> Vec<FieldNode> {
    let props = object_properties(schema);
    fields_from_properties(&props, &required_set(schema), root, visited)
}

fn fields_from_properties(
    props: &[(String, Schema)],
    required: &std::collections::BTreeSet<String>,
    root: &RootSchema,
    visited: &mut std::collections::BTreeSet<String>,
) -> Vec<FieldNode> {
    let mut fields: Vec<FieldNode> = props
        .iter()
        .filter(|(name, _)| !matches!(name.as_str(), "apiVersion" | "kind" | "metadata" | "status"))
        .map(|(name, schema)| field_node(name, schema, required.contains(name), root, visited))
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
    schema: &Schema,
    required: bool,
    root: &RootSchema,
    visited: &mut std::collections::BTreeSet<String>,
) -> FieldNode {
    let descent = RefDescent::enter(schema, visited);
    let resolved = resolve_ref(schema, root);
    let description = schema_description(schema)
        .or_else(|| schema_description(&Schema::Object(resolved.clone())))
        .unwrap_or_default();
    let type_desc = type_description(&resolved, root, visited);
    // A `$ref` re-entry (cycle) stops here: emit the field as a leaf.
    let children = if is_object(&resolved) && descent.safe() {
        object_fields(&resolved, root, visited)
    } else {
        Vec::new()
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

/// Pull a `description` out of a schema's metadata, if present.
fn schema_description(schema: &Schema) -> Option<String> {
    let Schema::Object(obj) = schema else {
        return None;
    };
    obj.metadata
        .as_ref()
        .and_then(|m| m.description.clone())
        .filter(|d| !d.is_empty())
}

/// True when the (resolved) schema describes a JSON object with properties.
fn is_object(schema: &SchemaObject) -> bool {
    schema
        .object
        .as_ref()
        .map(|o| !o.properties.is_empty())
        .unwrap_or(false)
}

/// Map a (resolved) schema to cfgd's type description: `[]<inner>` for arrays,
/// `object` for objects/maps, otherwise the JSON instance type (`string`,
/// `integer`, `boolean`, …). Falls back to `object` when no type is declared
/// (e.g. enums, untyped maps).
fn type_description(
    schema: &SchemaObject,
    root: &RootSchema,
    visited: &mut std::collections::BTreeSet<String>,
) -> String {
    if let Some(array) = &schema.array
        && let Some(items) = &array.items
    {
        let inner = match items {
            SingleOrVec::Single(item) => array_inner_type(item, root, visited),
            SingleOrVec::Vec(items) => items
                .first()
                .map(|item| array_inner_type(item, root, visited))
                .unwrap_or_else(|| "string".to_string()),
        };
        return format!("[]{inner}");
    }
    if is_object(schema) {
        return "object".to_string();
    }
    match &schema.instance_type {
        Some(SingleOrVec::Single(t)) => instance_type_name(t),
        Some(SingleOrVec::Vec(types)) => types
            .iter()
            .map(instance_type_name)
            .find(|name| name != "null")
            .unwrap_or_else(|| "object".to_string()),
        None => "object".to_string(),
    }
}

/// Type description of an array element, guarding the element `$ref` against a
/// cycle (a `Vec` whose element type `$ref`s back onto the descent path renders
/// as `object` rather than recursing).
fn array_inner_type(
    item: &Schema,
    root: &RootSchema,
    visited: &mut std::collections::BTreeSet<String>,
) -> String {
    let descent = RefDescent::enter(item, visited);
    let resolved = resolve_ref(item, root);
    let desc = if descent.safe() {
        type_description(&resolved, root, visited)
    } else {
        "object".to_string()
    };
    descent.leave(visited);
    desc
}

fn instance_type_name(t: &schemars::schema::InstanceType) -> String {
    use schemars::schema::InstanceType::*;
    match t {
        Null => "null",
        Boolean => "boolean",
        Object => "object",
        Array => "array",
        Number => "number",
        String => "string",
        Integer => "integer",
    }
    .to_string()
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
}
