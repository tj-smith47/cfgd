//! CRD YAML generation for the Helm chart.
//!
//! Sources the CRD spec types from `cfgd-crd` (via the operator re-export)
//! and renders kube's `CustomResourceExt::crd()` output to YAML, injecting the
//! `x-kubernetes-list-type` / CEL structural-merge annotations that schemars
//! cannot express. [`render_all`] is the testable library entry point; the
//! `cfgd-gen-crds` binary wraps it for stdout / file-tree emission.

use kube::CustomResourceExt;
use thiserror::Error;

use crate::crds::{
    BackupPolicy, ClusterConfigPolicy, ConfigPolicy, DriftAlert, MachineConfig, Module,
};

/// Failure rendering a CRD to YAML. Both arms are infallible in practice (the
/// CRD shapes are derived, not user-supplied) but the rule against `expect` in
/// library code is absolute, so the serde errors propagate as a typed result.
#[derive(Debug, Error)]
pub enum GenCrdsError {
    #[error("serialize CRD to JSON: {0}")]
    Json(#[from] serde_json::Error),

    #[error("serialize CRD to YAML: {0}")]
    Yaml(#[from] serde_yaml::Error),
}

/// A single rendered CRD: its `metadata.name` (e.g. `machineconfigs.cfgd.io`)
/// and its serialized YAML document.
pub struct RenderedCrd {
    pub name: String,
    pub yaml: String,
}

/// Render one CRD value to YAML after injecting structural-merge / CEL metadata.
fn render_crd(mut crd: serde_json::Value, inject_cel: bool) -> Result<RenderedCrd, GenCrdsError> {
    if inject_cel {
        inject_cel_rules(&mut crd);
    }
    inject_smd_annotations(&mut crd);
    if let Some(schema) = crd.pointer_mut("/spec/versions/0/schema/openAPIV3Schema") {
        sanitize_structural(schema);
    }
    let name = crd
        .pointer("/metadata/name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let yaml = serde_yaml::to_string(&crd)?;
    Ok(RenderedCrd { name, yaml })
}

/// Render every CRD, each as a [`RenderedCrd`], in the chart's canonical order
/// (MachineConfig, ConfigPolicy, DriftAlert, ClusterConfigPolicy, Module,
/// BackupPolicy).
pub fn render_each() -> Result<Vec<RenderedCrd>, GenCrdsError> {
    Ok(vec![
        // MachineConfig is the only kind carrying the hostname / files CEL rules.
        render_crd(serde_json::to_value(MachineConfig::crd())?, true)?,
        render_crd(serde_json::to_value(ConfigPolicy::crd())?, false)?,
        render_crd(serde_json::to_value(DriftAlert::crd())?, false)?,
        render_crd(serde_json::to_value(ClusterConfigPolicy::crd())?, false)?,
        render_crd(serde_json::to_value(Module::crd())?, false)?,
        render_crd(serde_json::to_value(BackupPolicy::crd())?, false)?,
    ])
}

/// Render every CRD into a single `---\n`-joined YAML document — the exact
/// bytes the `cfgd-gen-crds` binary emits on stdout for the Helm chart.
pub fn render_all() -> Result<String, GenCrdsError> {
    let docs = render_each()?;
    let joined = docs
        .iter()
        .map(|c| c.yaml.as_str())
        .collect::<Vec<_>>()
        .join("---\n");
    Ok(joined)
}

/// Fold a schemars-derived schema into a Kubernetes STRUCTURAL schema.
///
/// The API server refuses a CRD whose schema is not structural, so two shapes
/// schemars emits have to be folded away before the document is written:
///
/// - `additionalProperties: false`, which `#[serde(deny_unknown_fields)]`
///   produces. Kubernetes rejects it wherever `properties` is also set, and
///   the CRD's own pruning already drops unknown fields, so the constraint is
///   redundant as well as illegal. A node left with no type by that removal
///   settles as `type: object` — it asked for LESS than the default, so it
///   must never fall through to the free-form marker below.
/// - a schema node with no single OpenAPI type: the free-form `patch.ensure`
///   arm, which carries no `type` at all, and the untagged `ScriptEntry`
///   union, whose `anyOf` carries an empty arm the `type` schemars emits
///   beside it contradicts. Those become
///   `x-kubernetes-preserve-unknown-fields: true`, the API server's own
///   spelling for "keep whatever is here and validate no further".
///
/// The walk descends only through the child positions that are themselves
/// schemas (`properties`, `items`, `additionalProperties`, `patternProperties`)
/// — the same positions the structural rules require a `type` at. A blind walk
/// over every nested object would reach a `default: {}` literal or an
/// `x-kubernetes-validations` rule and rewrite data that is not a schema.
/// Whether a union node carries the tell of kube's own flattening: an arm that
/// is the EMPTY schema, matching anything at all.
///
/// schemars renders an untagged enum as its object arm's own `type` and
/// `properties` alongside an `anyOf` naming every arm, and that `type` is then
/// a claim the union does not keep — `ScriptEntry`'s bare-string arm is not an
/// object, so the API server rejects exactly the shorthand the local YAML
/// documents. The empty arm is what says the union spans more than the type
/// beside it; a union whose arms all agree on a type keeps the claim.
fn union_flattens_over_an_untyped_arm(map: &serde_json::Map<String, serde_json::Value>) -> bool {
    ["anyOf", "oneOf"].iter().any(|key| {
        map.get(*key)
            .and_then(serde_json::Value::as_array)
            .is_some_and(|arms| {
                arms.iter()
                    .any(|arm| arm.as_object().is_some_and(serde_json::Map::is_empty))
            })
    })
}

fn sanitize_structural(schema: &mut serde_json::Value) {
    let Some(map) = schema.as_object_mut() else {
        return;
    };
    let denied_unknown = map.get("additionalProperties") == Some(&serde_json::Value::Bool(false));
    if denied_unknown {
        map.remove("additionalProperties");
    }
    if union_flattens_over_an_untyped_arm(map) {
        map.remove("type");
    }
    if !map.contains_key("type") && !map.contains_key("$ref") && !map.contains_key("allOf") {
        let settled = if denied_unknown {
            // The node said "no field but the ones I list"; dropping that on
            // the floor and marking it free-form inverts the author's
            // intent. `type: object` restores it — the CRD's own pruning
            // already removes every unlisted field.
            ("type", serde_json::Value::String("object".to_string()))
        } else {
            (
                "x-kubernetes-preserve-unknown-fields",
                serde_json::Value::Bool(true),
            )
        };
        map.insert(settled.0.to_string(), settled.1);
    }
    for key in ["properties", "patternProperties"] {
        if let Some(children) = map.get_mut(key).and_then(serde_json::Value::as_object_mut) {
            for child in children.values_mut() {
                sanitize_structural(child);
            }
        }
    }
    for key in ["items", "additionalProperties"] {
        if let Some(child) = map.get_mut(key)
            && child.is_object()
        {
            sanitize_structural(child);
        }
    }
}

fn inject_smd_annotations(crd: &mut serde_json::Value) {
    let spec_base = "/spec/versions/0/schema/openAPIV3Schema/properties";

    // conditions lists: merge by "type" key
    let conditions_paths = [format!("{spec_base}/status/properties/conditions")];
    for path in &conditions_paths {
        if let Some(conditions) = crd.pointer_mut(path) {
            conditions["x-kubernetes-list-type"] = serde_json::json!("map");
            conditions["x-kubernetes-list-map-keys"] = serde_json::json!(["type"]);
        }
    }

    // packages list: merge by "name" key
    if let Some(packages) = crd.pointer_mut(&format!("{spec_base}/spec/properties/packages")) {
        packages["x-kubernetes-list-type"] = serde_json::json!("map");
        packages["x-kubernetes-list-map-keys"] = serde_json::json!(["name"]);
    }

    // moduleRefs list: merge by "name" key (MachineConfig only)
    if let Some(refs) = crd.pointer_mut(&format!("{spec_base}/spec/properties/moduleRefs")) {
        refs["x-kubernetes-list-type"] = serde_json::json!("map");
        refs["x-kubernetes-list-map-keys"] = serde_json::json!(["name"]);
    }

    // requiredModules list: merge by "name" key (ConfigPolicy/ClusterConfigPolicy)
    if let Some(refs) = crd.pointer_mut(&format!("{spec_base}/spec/properties/requiredModules")) {
        refs["x-kubernetes-list-type"] = serde_json::json!("map");
        refs["x-kubernetes-list-map-keys"] = serde_json::json!(["name"]);
    }

    // debugModules list: merge by "name" key (ConfigPolicy/ClusterConfigPolicy)
    if let Some(refs) = crd.pointer_mut(&format!("{spec_base}/spec/properties/debugModules")) {
        refs["x-kubernetes-list-type"] = serde_json::json!("map");
        refs["x-kubernetes-list-map-keys"] = serde_json::json!(["name"]);
    }

    // files list: merge by map key — "path" for MachineConfig, "target" for Module
    if let Some(files) = crd.pointer_mut(&format!("{spec_base}/spec/properties/files")) {
        files["x-kubernetes-list-type"] = serde_json::json!("map");
        // Determine map key from the items schema: Module files have "source"+"target",
        // MachineConfig files have "path"+"content"+"source"+"mode".
        //
        // A Module file is keyed by its TARGET, the one field every entry
        // carries: a `strategy: Patch` entry rewrites the target in place and
        // declares no source at all, so several of them in one module would
        // collide on an empty `source` and the API server would refuse the
        // whole resource.
        let has_path_property = files.pointer("/items/properties/path").is_some();
        if has_path_property {
            files["x-kubernetes-list-map-keys"] = serde_json::json!(["path"]);
        } else {
            files["x-kubernetes-list-map-keys"] = serde_json::json!(["target"]);
        }
    }

    // units lists (BackupPolicy only): the policy's own overrides merge by the
    // unit name; the status carries one row per (machine, unit), so its key is
    // the pair — several machines report the same unit name, and a map list
    // whose keys repeat is refused by the API server outright.
    if let Some(units) = crd.pointer_mut(&format!("{spec_base}/spec/properties/units")) {
        units["x-kubernetes-list-type"] = serde_json::json!("map");
        units["x-kubernetes-list-map-keys"] = serde_json::json!(["name"]);
    }
    if let Some(units) = crd.pointer_mut(&format!("{spec_base}/status/properties/units")) {
        units["x-kubernetes-list-type"] = serde_json::json!("map");
        units["x-kubernetes-list-map-keys"] = serde_json::json!(["hostname", "name"]);
    }

    // driftDetails list: merge by "field" key (DriftAlert only)
    if let Some(details) = crd.pointer_mut(&format!("{spec_base}/spec/properties/driftDetails")) {
        details["x-kubernetes-list-type"] = serde_json::json!("map");
        details["x-kubernetes-list-map-keys"] = serde_json::json!(["field"]);
    }

    // matchExpressions: merge by "key"
    for selector_path in &["targetSelector", "namespaceSelector"] {
        let path =
            format!("{spec_base}/spec/properties/{selector_path}/properties/matchExpressions");
        if let Some(exprs) = crd.pointer_mut(&path) {
            exprs["x-kubernetes-list-type"] = serde_json::json!("map");
            exprs["x-kubernetes-list-map-keys"] = serde_json::json!(["key"]);
        }
    }

    // trustedRegistries: atomic set
    if let Some(registries) = crd.pointer_mut(&format!(
        "{spec_base}/spec/properties/security/properties/trustedRegistries"
    )) {
        registries["x-kubernetes-list-type"] = serde_json::json!("set");
    }

    // Module: env list — merge by "name" key
    if let Some(env) = crd.pointer_mut(&format!("{spec_base}/spec/properties/env")) {
        env["x-kubernetes-list-type"] = serde_json::json!("map");
        env["x-kubernetes-list-map-keys"] = serde_json::json!(["name"]);
    }

    // Module: depends — atomic set
    if let Some(depends) = crd.pointer_mut(&format!("{spec_base}/spec/properties/depends")) {
        depends["x-kubernetes-list-type"] = serde_json::json!("set");
    }
}

fn inject_cel_rules(crd: &mut serde_json::Value) {
    if let Some(spec) = crd.pointer_mut("/spec/versions/0/schema/openAPIV3Schema/properties/spec") {
        spec["x-kubernetes-validations"] = serde_json::json!([
            {
                "rule": "self.hostname.size() > 0",
                "message": "hostname must not be empty"
            }
        ]);
        if let Some(files_items) = spec.pointer_mut("/properties/files/items") {
            files_items["x-kubernetes-validations"] = serde_json::json!([
                {
                    "rule": "has(self.content) || has(self.source)",
                    "message": "each file must have content or source"
                }
            ]);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{inject_cel_rules, inject_smd_annotations, render_all};
    use serde_json::{Value, json};

    /// How many CRD kinds the registry carries — the count every walk over the
    /// rendered documents holds itself to, so a kind added to `cfgd-crd`
    /// widens the walks instead of leaving them passing over a stale number.
    fn registered_crd_kinds() -> usize {
        cfgd_core::schema::KIND_REGISTRY
            .iter()
            .filter(|e| e.crd)
            .count()
    }

    #[test]
    fn render_all_covers_every_crd() {
        let yaml = render_all().expect("render CRDs");
        for k in [
            "machineconfigs",
            "configpolicies",
            "clusterconfigpolicies",
            "driftalerts",
            "modules",
            "backuppolicies",
        ] {
            assert!(
                yaml.contains(&format!("name: {k}.cfgd.io")),
                "missing CRD {k}"
            );
        }
    }

    #[test]
    fn render_all_preserves_dashed_document_separator() {
        let yaml = render_all().expect("render CRDs");
        let docs = super::render_each().expect("render CRDs");
        // N documents joined by `---\n` => exactly N-1 separators.
        assert_eq!(yaml.matches("---\n").count(), docs.len() - 1);
    }

    /// Read a workspace file, panicking by name when it cannot be read.
    ///
    /// A roster this walk cannot read is a roster it cannot judge, and a walk
    /// that skips what it cannot read is a walk that passes on an empty tree.
    fn roster_file(relative: &str) -> String {
        let path = cfgd_core::test_helpers::workspace_root().join(relative);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
    }

    /// The `["a", "b"]` inline list a chart RBAC or roster line ends on.
    fn inline_list(line: &str, file: &str) -> BTreeSet<String> {
        let open = line
            .find('[')
            .unwrap_or_else(|| panic!("{file}: no inline list on line: {line}"));
        let close = line
            .rfind(']')
            .unwrap_or_else(|| panic!("{file}: unterminated inline list on line: {line}"));
        line[open + 1..close]
            .split(',')
            .map(|item| item.trim().trim_matches('"').to_string())
            .filter(|item| !item.is_empty())
            .collect()
    }

    /// The three `cfgd.io` resource lists of a ClusterRole template, each with
    /// its subresource suffix stripped, so all three answer the plural set.
    ///
    /// The chart templates are Helm, not YAML, so the rule is read off the raw
    /// line rather than through a parser that would choke on `{{ … }}`.
    fn cfgd_rbac_resource_sets(relative: &str) -> Vec<BTreeSet<String>> {
        let body = roster_file(relative);
        let mut sets = Vec::new();
        let mut in_cfgd_rule = false;
        for line in body.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("- apiGroups:") {
                in_cfgd_rule = trimmed.contains("\"cfgd.io\"");
                continue;
            }
            if in_cfgd_rule && trimmed.starts_with("resources:") {
                sets.push(
                    inline_list(trimmed, relative)
                        .iter()
                        .map(|r| {
                            r.split('/')
                                .next()
                                .unwrap_or_else(|| panic!("{relative}: empty resource name"))
                                .to_string()
                        })
                        .collect(),
                );
            }
        }
        assert_eq!(
            sets.len(),
            3,
            "{relative} must carry three cfgd.io resource rules (CRUD, /status, /finalizers)"
        );
        sets
    }

    /// Every CRD kind the registry carries is rendered exactly once, and every
    /// hand-maintained roster a kind must join names exactly what was rendered.
    ///
    /// Each roster is a list somebody edits by hand, and a kind missing from one
    /// is silent everywhere else in the tree: `kubectl apply -k` installs only
    /// what `kustomization.yaml` names, a kind absent from `webhook-config.yaml`
    /// is admitted into a real cluster with no validation at all, and
    /// `rbac_parity` proves only that the chart and the CSV agree — both can
    /// lack the same kind together.
    #[test]
    fn every_crd_kind_in_the_registry_is_rendered() {
        let registered: BTreeSet<&str> = cfgd_core::schema::KIND_REGISTRY
            .iter()
            .filter(|e| e.crd)
            .map(|e| e.kind)
            .collect();
        assert!(
            registered.len() >= 6,
            "the registry carries only {} CRD kinds, so this walk proves nothing",
            registered.len()
        );

        let docs = super::render_each().expect("render CRDs");
        let mut rendered: BTreeSet<String> = BTreeSet::new();
        let mut plurals: BTreeSet<String> = BTreeSet::new();
        for doc in &docs {
            let crd: Value = serde_yaml::from_str(&doc.yaml).expect("parse rendered CRD");
            let kind = crd["spec"]["names"]["kind"]
                .as_str()
                .unwrap_or_else(|| panic!("{} declares no spec.names.kind", doc.name))
                .to_string();
            assert!(
                rendered.insert(kind.clone()),
                "{kind} is rendered more than once"
            );
            let plural = doc
                .name
                .strip_suffix(".cfgd.io")
                .unwrap_or_else(|| panic!("{} is not a cfgd.io CRD name", doc.name));
            plurals.insert(plural.to_string());
        }
        assert_eq!(
            rendered.iter().map(String::as_str).collect::<BTreeSet<_>>(),
            registered,
            "every CRD kind in the registry is rendered, and nothing else is"
        );
        // The webhook path a kind registers under is its own kind lowercased —
        // the spelling `webhook/mod.rs` routes and `webhook-config.yaml` names.
        let singulars: BTreeSet<String> = rendered.iter().map(|k| k.to_lowercase()).collect();
        let crd_files: BTreeSet<String> = plurals.iter().map(|p| format!("{p}.yaml")).collect();
        let crd_names: BTreeSet<String> = plurals.iter().map(|p| format!("{p}.cfgd.io")).collect();

        let kustomization = "chart/cfgd/crds/kustomization.yaml";
        let listed: BTreeSet<String> = serde_yaml::from_str::<Value>(&roster_file(kustomization))
            .unwrap_or_else(|e| panic!("cannot parse {kustomization}: {e}"))
            .get("resources")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("{kustomization} carries no resources list"))
            .iter()
            .map(|r| {
                r.as_str()
                    .unwrap_or_else(|| panic!("{kustomization} lists a non-string resource"))
                    .to_string()
            })
            .collect();
        assert_eq!(
            listed, crd_files,
            "{kustomization} must list exactly the rendered CRDs — \
             `kubectl apply -k` installs only what it names"
        );

        let csv = "ecosystem/olm/manifests/cfgd-operator.clusterserviceversion.yaml";
        let csv_doc = serde_yaml::from_str::<Value>(&roster_file(csv))
            .unwrap_or_else(|e| panic!("cannot parse {csv}: {e}"));
        let owned = csv_doc["spec"]["customresourcedefinitions"]["owned"]
            .as_array()
            .unwrap_or_else(|| panic!("{csv} carries no customresourcedefinitions.owned list"));
        let owned_names: BTreeSet<String> = owned
            .iter()
            .map(|e| {
                e["name"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{csv}: an owned entry declares no name"))
                    .to_string()
            })
            .collect();
        let owned_kinds: BTreeSet<String> = owned
            .iter()
            .map(|e| {
                e["kind"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{csv}: an owned entry declares no kind"))
                    .to_string()
            })
            .collect();
        assert_eq!(owned_names, crd_names, "{csv} owned: names every CRD");
        assert_eq!(owned_kinds, rendered, "{csv} owned: names every CRD's kind");

        let webhook_config = "chart/cfgd/templates/webhook-config.yaml";
        let body = roster_file(webhook_config);
        let mut hooked_singulars = BTreeSet::new();
        let mut hooked_plurals = BTreeSet::new();
        for line in body.lines() {
            let Some(rest) = line.trim().strip_prefix("(dict \"singular\" ") else {
                continue;
            };
            let mut quoted = rest.split('"').skip(1).step_by(2);
            let singular = quoted
                .next()
                .unwrap_or_else(|| panic!("{webhook_config}: a dict entry names no singular"));
            let plural = quoted
                .nth(1)
                .unwrap_or_else(|| panic!("{webhook_config}: a dict entry names no plural"));
            hooked_singulars.insert(singular.to_string());
            hooked_plurals.insert(plural.to_string());
        }
        assert_eq!(
            hooked_singulars, singulars,
            "{webhook_config} must register a validating webhook for every kind — \
             a kind it omits is admitted with no validation at all"
        );
        assert_eq!(
            hooked_plurals, plurals,
            "{webhook_config} must name every kind's plural"
        );

        for rbac in [
            "chart/cfgd/templates/rbac.yaml",
            "chart/cfgd/templates/rbac-examples/platform-admin.yaml",
        ] {
            for resources in cfgd_rbac_resource_sets(rbac) {
                assert_eq!(
                    resources, plurals,
                    "{rbac} must grant every cfgd.io resource, its /status and its /finalizers"
                );
            }
        }

        let taskfile = "Taskfile.yml";
        let body = roster_file(taskfile);
        let loop_line = body
            .lines()
            .map(str::trim)
            .find(|l| l.starts_with("for f in ") && l.ends_with("; do"))
            .unwrap_or_else(|| panic!("{taskfile}: gen:crds:check declares no file loop"));
        let looped: BTreeSet<String> = loop_line
            .trim_start_matches("for f in ")
            .trim_end_matches("; do")
            .split_whitespace()
            .map(str::to_string)
            .collect();
        assert_eq!(
            looped, plurals,
            "{taskfile}: gen:crds:check must diff every rendered CRD copy"
        );

        let connection = "chart/cfgd/templates/tests/test-connection.yaml";
        let body = roster_file(connection);
        let probed: BTreeSet<String> = body
            .lines()
            .filter_map(|l| l.trim().strip_prefix("kubectl get crd "))
            .filter_map(|l| l.split_whitespace().next())
            .map(str::to_string)
            .collect();
        assert_eq!(
            probed, crd_names,
            "{connection} must check every CRD is established"
        );
    }

    /// Split a printer-column jsonPath into `(field, is_indexed)` segments,
    /// dropping whatever sits inside `[...]`: a filter or an index only says
    /// the field it follows is a list, and the schema walk descends into that
    /// list's `items` for the segment after it.
    fn path_segments(json_path: &str) -> Vec<(String, bool)> {
        let mut segments = Vec::new();
        let mut current = String::new();
        let mut indexed = false;
        let mut depth = 0usize;
        for ch in json_path.chars() {
            match ch {
                '[' => {
                    depth += 1;
                    indexed = true;
                }
                ']' => depth = depth.saturating_sub(1),
                _ if depth > 0 => {}
                '.' => {
                    if !current.is_empty() {
                        segments.push((std::mem::take(&mut current), indexed));
                        indexed = false;
                    }
                }
                c => current.push(c),
            }
        }
        if !current.is_empty() {
            segments.push((current, indexed));
        }
        segments
    }

    /// The OpenAPI `type` a printer column's jsonPath resolves to, or `None`
    /// when the schema does not describe that path at all.
    fn schema_type_at(schema: &Value, json_path: &str) -> Option<String> {
        let mut node = schema;
        for (field, indexed) in path_segments(json_path) {
            node = node.get("properties")?.get(&field)?;
            if indexed {
                node = node.get("items")?;
            }
        }
        node.get("type").and_then(Value::as_str).map(str::to_string)
    }

    /// Walk every `additionalPrinterColumns` entry on every CRD and refuse any
    /// column bound to a list. The API server's table converter hands the
    /// column's raw JSON value to `fmt.Sprintf("%v", …)`, so an array renders
    /// as the Go slice syntax — `[]` for the empty case, where an absent value
    /// leaves the cell empty. A column showing a list shows a
    /// string the controller derived from it (`Module.status.platformsSummary`
    /// is the worked example).
    #[test]
    fn no_printer_column_is_bound_to_a_list() {
        let docs = super::render_each().expect("render CRDs");
        let mut checked = 0usize;
        for doc in &docs {
            let crd: Value = serde_yaml::from_str(&doc.yaml).expect("parse rendered CRD");
            let version = &crd["spec"]["versions"][0];
            let schema = &version["schema"]["openAPIV3Schema"];
            let columns = version["additionalPrinterColumns"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            assert!(
                !columns.is_empty(),
                "{} declares no printer columns",
                doc.name
            );
            for column in &columns {
                let path = column["jsonPath"].as_str().expect("jsonPath is a string");
                let name = column["name"].as_str().unwrap_or_default();
                match schema_type_at(schema, path) {
                    Some(kind) => {
                        assert!(
                            kind != "array" && kind != "object",
                            "{}: column {name} ({path}) resolves to a {kind}; \
                             bind it to a scalar the controller derives",
                            doc.name
                        );
                        checked += 1;
                    }
                    // `metadata` is rendered as an opaque object by kube, so
                    // nothing under it can be resolved — and nothing under it
                    // is a list either. Any OTHER unresolvable path is a
                    // column pointing at a field that does not exist.
                    None => assert!(
                        path.starts_with(".metadata."),
                        "{}: column {name} ({path}) resolves to no schema field",
                        doc.name
                    ),
                }
            }
        }
        assert!(
            checked >= docs.len(),
            "the walk resolved almost nothing ({checked} columns) — it is passing vacuously"
        );
    }

    // Build a fully-populated CRD-shaped serde_json::Value that exercises every
    // pointer_mut path in inject_smd_annotations + inject_cel_rules. Each
    // top-level test below mutates a clone of the result so the assertions
    // stay independent.
    fn full_crd_shape() -> Value {
        json!({
            "spec": {
                "versions": [{
                    "schema": {
                        "openAPIV3Schema": {
                            "properties": {
                                "spec": {
                                    "properties": {
                                        "packages": {"items": {}},
                                        "moduleRefs": {"items": {}},
                                        "requiredModules": {"items": {}},
                                        "debugModules": {"items": {}},
                                        "driftDetails": {"items": {}},
                                        "units": {"items": {}},
                                        "env": {"items": {}},
                                        "depends": {"items": {}},
                                        "files": {
                                            "items": {
                                                "properties": {
                                                    "path": {},
                                                    "content": {},
                                                    "mode": {}
                                                }
                                            }
                                        },
                                        "targetSelector": {
                                            "properties": {
                                                "matchExpressions": {"items": {}}
                                            }
                                        },
                                        "namespaceSelector": {
                                            "properties": {
                                                "matchExpressions": {"items": {}}
                                            }
                                        },
                                        "security": {
                                            "properties": {
                                                "trustedRegistries": {"items": {"type": "string"}}
                                            }
                                        }
                                    }
                                },
                                "status": {
                                    "properties": {
                                        "conditions": {"items": {}},
                                        "units": {"items": {}}
                                    }
                                }
                            }
                        }
                    }
                }]
            }
        })
    }

    fn smd(value: &Value, ptr: &str, key: &str) -> Option<Value> {
        value.pointer(ptr).and_then(|n| n.get(key)).cloned()
    }

    #[test]
    fn inject_smd_annotations_marks_every_known_list_field_with_map_or_set_metadata() {
        let mut crd = full_crd_shape();
        inject_smd_annotations(&mut crd);

        let base = "/spec/versions/0/schema/openAPIV3Schema/properties";

        // conditions: map by "type"
        let conditions = format!("{base}/status/properties/conditions");
        assert_eq!(
            smd(&crd, &conditions, "x-kubernetes-list-type"),
            Some(json!("map"))
        );
        assert_eq!(
            smd(&crd, &conditions, "x-kubernetes-list-map-keys"),
            Some(json!(["type"]))
        );

        // packages: map by "name"
        let packages = format!("{base}/spec/properties/packages");
        assert_eq!(
            smd(&crd, &packages, "x-kubernetes-list-type"),
            Some(json!("map"))
        );
        assert_eq!(
            smd(&crd, &packages, "x-kubernetes-list-map-keys"),
            Some(json!(["name"]))
        );

        // moduleRefs / requiredModules / debugModules: map by "name"
        for field in ["moduleRefs", "requiredModules", "debugModules", "env"] {
            let path = format!("{base}/spec/properties/{field}");
            assert_eq!(
                smd(&crd, &path, "x-kubernetes-list-type"),
                Some(json!("map")),
                "{field} list-type"
            );
            assert_eq!(
                smd(&crd, &path, "x-kubernetes-list-map-keys"),
                Some(json!(["name"])),
                "{field} list-map-keys"
            );
        }

        // units: the spec's by name, the status's by the (hostname, name) pair
        // one machine's copy of a unit is identified by.
        let spec_units = format!("{base}/spec/properties/units");
        assert_eq!(
            smd(&crd, &spec_units, "x-kubernetes-list-type"),
            Some(json!("map"))
        );
        assert_eq!(
            smd(&crd, &spec_units, "x-kubernetes-list-map-keys"),
            Some(json!(["name"]))
        );
        let status_units = format!("{base}/status/properties/units");
        assert_eq!(
            smd(&crd, &status_units, "x-kubernetes-list-type"),
            Some(json!("map"))
        );
        assert_eq!(
            smd(&crd, &status_units, "x-kubernetes-list-map-keys"),
            Some(json!(["hostname", "name"]))
        );

        // driftDetails: map by "field"
        let drift = format!("{base}/spec/properties/driftDetails");
        assert_eq!(
            smd(&crd, &drift, "x-kubernetes-list-type"),
            Some(json!("map"))
        );
        assert_eq!(
            smd(&crd, &drift, "x-kubernetes-list-map-keys"),
            Some(json!(["field"]))
        );

        // matchExpressions on both selectors: map by "key"
        for selector in ["targetSelector", "namespaceSelector"] {
            let path = format!("{base}/spec/properties/{selector}/properties/matchExpressions");
            assert_eq!(
                smd(&crd, &path, "x-kubernetes-list-type"),
                Some(json!("map")),
                "{selector} list-type"
            );
            assert_eq!(
                smd(&crd, &path, "x-kubernetes-list-map-keys"),
                Some(json!(["key"])),
                "{selector} list-map-keys"
            );
        }

        // trustedRegistries: set (no map keys)
        let registries = format!("{base}/spec/properties/security/properties/trustedRegistries");
        assert_eq!(
            smd(&crd, &registries, "x-kubernetes-list-type"),
            Some(json!("set"))
        );
        assert!(
            crd.pointer(&format!("{registries}/x-kubernetes-list-map-keys"))
                .is_none(),
            "trustedRegistries is a set and must NOT have map-keys"
        );

        // depends: set (no map keys)
        let depends = format!("{base}/spec/properties/depends");
        assert_eq!(
            smd(&crd, &depends, "x-kubernetes-list-type"),
            Some(json!("set"))
        );
        assert!(
            crd.pointer(&format!("{depends}/x-kubernetes-list-map-keys"))
                .is_none()
        );
    }

    #[test]
    fn inject_smd_annotations_files_list_keys_by_path_when_items_have_path_property() {
        // MachineConfig + ConfigPolicy shape: items.properties.path exists.
        let mut crd = full_crd_shape();
        inject_smd_annotations(&mut crd);

        let base = "/spec/versions/0/schema/openAPIV3Schema/properties";
        let files = format!("{base}/spec/properties/files");
        assert_eq!(
            smd(&crd, &files, "x-kubernetes-list-type"),
            Some(json!("map"))
        );
        assert_eq!(
            smd(&crd, &files, "x-kubernetes-list-map-keys"),
            Some(json!(["path"]))
        );
    }

    #[test]
    fn inject_smd_annotations_files_list_keys_by_target_when_items_lack_path_property() {
        // Module shape: items.properties has "source"+"target" but not "path".
        // A `strategy: Patch` entry declares no source, so `target` is the one
        // field every entry fills and the only safe server-side merge key.
        let mut crd = json!({
            "spec": {"versions": [{"schema": {"openAPIV3Schema": {"properties": {"spec": {
                "properties": {
                    "files": {
                        "items": {"properties": {"source": {}, "target": {}}}
                    }
                }
            }}}}}]}
        });
        inject_smd_annotations(&mut crd);

        let files = "/spec/versions/0/schema/openAPIV3Schema/properties/spec/properties/files";
        assert_eq!(
            smd(&crd, files, "x-kubernetes-list-type"),
            Some(json!("map"))
        );
        assert_eq!(
            smd(&crd, files, "x-kubernetes-list-map-keys"),
            Some(json!(["target"]))
        );
    }

    #[test]
    fn inject_smd_annotations_is_no_op_when_optional_fields_are_absent() {
        // DriftAlert-ish minimal shape: only conditions + driftDetails present.
        // Every other `if let Some(...)` arm must skip silently without panicking.
        let mut crd = json!({
            "spec": {"versions": [{"schema": {"openAPIV3Schema": {"properties": {
                "spec": {"properties": {"driftDetails": {"items": {}}}},
                "status": {"properties": {"conditions": {"items": {}}}}
            }}}}]}
        });
        inject_smd_annotations(&mut crd);

        let base = "/spec/versions/0/schema/openAPIV3Schema/properties";
        assert_eq!(
            smd(
                &crd,
                &format!("{base}/status/properties/conditions"),
                "x-kubernetes-list-type"
            ),
            Some(json!("map"))
        );
        assert_eq!(
            smd(
                &crd,
                &format!("{base}/spec/properties/driftDetails"),
                "x-kubernetes-list-type"
            ),
            Some(json!("map"))
        );
        // Unrelated fields stay absent — the function did not invent them.
        assert!(
            crd.pointer(&format!("{base}/spec/properties/packages"))
                .is_none()
        );
        assert!(
            crd.pointer(&format!("{base}/spec/properties/files"))
                .is_none()
        );
        assert!(
            crd.pointer(&format!("{base}/spec/properties/depends"))
                .is_none()
        );
    }

    /// Walk every rendered document for `additionalProperties: false`, which
    /// `#[serde(deny_unknown_fields)]` on a shared value type can reintroduce
    /// at any schemars upgrade. The API server refuses a CRD carrying it
    /// beside `properties`, and a CRD that fails to install is not something
    /// any unit test downstream of the render would notice.
    #[test]
    fn no_rendered_crd_carries_additional_properties_false() {
        fn find_false(node: &Value, path: &str, hits: &mut Vec<String>) {
            match node {
                Value::Object(map) => {
                    for (key, value) in map {
                        if key == "additionalProperties" && value == &Value::Bool(false) {
                            hits.push(path.to_string());
                        }
                        find_false(value, &format!("{path}.{key}"), hits);
                    }
                }
                Value::Array(items) => {
                    for (i, value) in items.iter().enumerate() {
                        find_false(value, &format!("{path}[{i}]"), hits);
                    }
                }
                _ => {}
            }
        }

        let docs = super::render_each().expect("render CRDs");
        assert_eq!(
            docs.len(),
            registered_crd_kinds(),
            "every CRD must be walked"
        );
        for doc in &docs {
            let crd: Value = serde_yaml::from_str(&doc.yaml).expect("parse rendered CRD");
            let mut hits = Vec::new();
            find_false(&crd, "", &mut hits);
            assert!(
                hits.is_empty(),
                "{} carries `additionalProperties: false` at {hits:?}; \
                 the API server rejects it alongside `properties`",
                doc.name
            );
        }
    }

    /// Every schema node the structural pass reaches, paired with its JSON
    /// pointer — the same descent `sanitize_structural` makes, so a walk pin
    /// judges exactly the nodes the pass judged and never a `default: {}`
    /// literal that is not a schema at all.
    fn schema_nodes<'a>(root: &'a Value, path: String, out: &mut Vec<(String, &'a Value)>) {
        let Some(map) = root.as_object() else {
            return;
        };
        out.push((path.clone(), root));
        for key in ["properties", "patternProperties"] {
            if let Some(children) = map.get(key).and_then(Value::as_object) {
                for (name, child) in children {
                    schema_nodes(child, format!("{path}/{key}/{name}"), out);
                }
            }
        }
        for key in ["items", "additionalProperties"] {
            if let Some(child) = map.get(key)
                && child.is_object()
            {
                schema_nodes(child, format!("{path}/{key}"), out);
            }
        }
    }

    /// Every rendered schema node, over every rendered document.
    fn every_rendered_schema_node() -> Vec<(String, Value)> {
        let docs = super::render_each().expect("render CRDs");
        assert_eq!(
            docs.len(),
            registered_crd_kinds(),
            "every CRD must be walked"
        );
        let mut all = Vec::new();
        for doc in &docs {
            let crd: Value = serde_yaml::from_str(&doc.yaml).expect("parse rendered CRD");
            let Some(root) = crd.pointer("/spec/versions/0/schema/openAPIV3Schema") else {
                panic!("{} carries no schema", doc.name);
            };
            let mut nodes = Vec::new();
            schema_nodes(root, doc.name.clone(), &mut nodes);
            all.extend(nodes.into_iter().map(|(p, v)| (p, v.clone())));
        }
        all
    }

    /// A typeless schema node is what the API server refuses a CRD for, so
    /// every one the pass leaves behind carries the marker that makes it
    /// legal. `patch.ensure` — the free-form mapping a user merges into their
    /// own config file, whose keys no schema can enumerate — is the floor.
    #[test]
    fn every_typeless_rendered_node_is_marked_preserve_unknown_fields() {
        let mut typeless = 0usize;
        for (path, node) in every_rendered_schema_node() {
            if node.get("type").is_some() || node.get("$ref").is_some() {
                continue;
            }
            typeless += 1;
            assert_eq!(
                node.get("x-kubernetes-preserve-unknown-fields"),
                Some(&json!(true)),
                "{path} has no OpenAPI type and no preserve-unknown-fields marker: {node:?}"
            );
        }
        assert!(
            typeless > 0,
            "no typeless node was walked, so the pin proves nothing"
        );
    }

    /// The untagged `ScriptEntry` accepts a bare command string as readily as
    /// the mapping form, and schemars renders the mapping arm's `type: object`
    /// beside the `anyOf` naming both. Left alone, the API server enforces that
    /// `type` and rejects every module whose hook is written the short way —
    /// which is what `cfgd module push --apply` emits. The tell that a union
    /// spans more than that type is an EMPTY arm; a union whose arms agree on
    /// a type keeps its claim, so every node the pass untyped carries the tell.
    #[test]
    fn every_untyped_union_node_carries_the_empty_arm_that_untyped_it() {
        let mut unions = 0usize;
        for (path, node) in every_rendered_schema_node() {
            let arms = ["anyOf", "oneOf"]
                .iter()
                .filter_map(|k| node.get(*k))
                .filter_map(Value::as_array)
                .flatten()
                .collect::<Vec<_>>();
            if arms.is_empty() || node.get("type").is_some() {
                continue;
            }
            unions += 1;
            assert!(
                arms.iter()
                    .any(|arm| arm.as_object().is_some_and(serde_json::Map::is_empty)),
                "{path} was left untyped but no arm of its union is the empty schema: {node:?}"
            );
        }
        assert!(unions > 0, "no untyped union was walked (ScriptEntry's)");
    }

    /// The Module CRD's hook lists are the union above, end to end.
    #[test]
    fn a_union_typed_schema_keeps_no_type_its_arms_contradict() {
        let docs = super::render_each().expect("render CRDs");
        let module = docs
            .iter()
            .find(|d| d.name == "modules.cfgd.io")
            .expect("the Module CRD is rendered");
        let crd: Value = serde_yaml::from_str(&module.yaml).expect("parse rendered CRD");

        let hooks = crd
            .pointer("/spec/versions/0/schema/openAPIV3Schema/properties/spec/properties/hooks/properties")
            .and_then(Value::as_object)
            .expect("the hooks schema is rendered");
        assert_eq!(
            hooks.len(),
            6,
            "every lifecycle hook is rendered: {hooks:?}"
        );

        for (hook, schema) in hooks {
            let items = schema.get("items").expect("a hook list has items");
            assert!(
                items.get("anyOf").is_some(),
                "{hook} items keep both arms of the union: {items:?}"
            );
            assert!(
                items.get("type").is_none(),
                "{hook} items must claim no single type, or the bare-string arm is refused: {items:?}"
            );
            assert_eq!(
                items.get("x-kubernetes-preserve-unknown-fields"),
                Some(&json!(true)),
                "{hook} items must be marked preserve-unknown-fields: {items:?}"
            );
        }
    }

    /// A node that said "no field but the ones I list" must not come out of
    /// the pass saying the opposite. `additionalProperties: false` is illegal
    /// in a structural schema and has to go, but dropping it leaves the node
    /// typeless, and the free-form marker would then invert the author's
    /// intent — `type: object` keeps it, the CRD's own pruning doing the work
    /// the removed constraint asked for.
    #[test]
    fn a_node_denying_unknown_fields_settles_as_an_object_not_as_free_form() {
        let mut schema = json!({
            "properties": {
                "closed": { "additionalProperties": false },
                "open": {}
            }
        });
        super::sanitize_structural(&mut schema);

        let closed = &schema["properties"]["closed"];
        assert_eq!(
            closed,
            &json!({ "type": "object" }),
            "a deny-unknown node settles as a pruned object: {closed:?}"
        );
        assert_eq!(
            schema["properties"]["open"],
            json!({ "x-kubernetes-preserve-unknown-fields": true }),
            "a node that asked for nothing still becomes free-form"
        );
    }

    /// The tell that a union spans more than the type rendered beside it is an
    /// arm that is the EMPTY schema. A union whose arms all agree on a type
    /// keeps its claim — stripping `type` there would hand the API server a
    /// node it can enforce nothing about, and earn a free-form marker on a
    /// shape the author fully described.
    #[test]
    fn a_union_whose_arms_agree_on_a_type_keeps_it_and_a_flattened_one_does_not() {
        let mut schema = json!({
            "properties": {
                "typed_arms": {
                    "type": "object",
                    "anyOf": [
                        { "type": "object", "properties": { "a": { "type": "string" } } },
                        { "type": "object", "properties": { "b": { "type": "string" } } }
                    ]
                },
                "flattened": {
                    "type": "object",
                    "anyOf": [{}, { "required": ["run"] }]
                }
            }
        });
        super::sanitize_structural(&mut schema);

        let typed = &schema["properties"]["typed_arms"];
        assert_eq!(
            typed.get("type"),
            Some(&json!("object")),
            "a union whose arms agree on a type keeps the claim: {typed:?}"
        );
        assert_eq!(
            typed.get("x-kubernetes-preserve-unknown-fields"),
            None,
            "a fully described union earns no free-form marker: {typed:?}"
        );

        let flattened = &schema["properties"]["flattened"];
        assert_eq!(
            flattened.get("type"),
            None,
            "an empty arm means the union spans more than the type beside it: {flattened:?}"
        );
        assert_eq!(
            flattened.get("x-kubernetes-preserve-unknown-fields"),
            Some(&json!(true)),
            "the node the pass untyped has to carry the marker: {flattened:?}"
        );
    }

    #[test]
    fn inject_cel_rules_attaches_hostname_validation_when_spec_path_exists() {
        let mut crd = full_crd_shape();
        inject_cel_rules(&mut crd);

        let spec = "/spec/versions/0/schema/openAPIV3Schema/properties/spec";
        let rules = crd
            .pointer(&format!("{spec}/x-kubernetes-validations"))
            .expect("hostname validation should be attached");
        let arr = rules.as_array().expect("validations is an array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["rule"], json!("self.hostname.size() > 0"));
        assert_eq!(arr[0]["message"], json!("hostname must not be empty"));
    }

    #[test]
    fn inject_cel_rules_attaches_files_items_content_or_source_validation() {
        let mut crd = full_crd_shape();
        inject_cel_rules(&mut crd);

        let files_items =
            "/spec/versions/0/schema/openAPIV3Schema/properties/spec/properties/files/items";
        let rules = crd
            .pointer(&format!("{files_items}/x-kubernetes-validations"))
            .expect("files.items validation should be attached");
        let arr = rules.as_array().expect("validations is an array");
        assert_eq!(arr.len(), 1);
        assert_eq!(
            arr[0]["rule"],
            json!("has(self.content) || has(self.source)")
        );
        assert_eq!(
            arr[0]["message"],
            json!("each file must have content or source")
        );
    }

    #[test]
    fn inject_cel_rules_skips_files_validation_when_files_items_missing() {
        // ConfigPolicy-like: spec exists but spec/properties/files does not.
        let mut crd = json!({
            "spec": {"versions": [{"schema": {"openAPIV3Schema": {"properties": {"spec": {
                "properties": {"hostname": {"type": "string"}}
            }}}}}]}
        });
        inject_cel_rules(&mut crd);

        let spec = "/spec/versions/0/schema/openAPIV3Schema/properties/spec";
        // The hostname rule should still attach
        assert!(
            crd.pointer(&format!("{spec}/x-kubernetes-validations"))
                .is_some()
        );
        // No files.items validation invented
        assert!(crd.pointer(&format!("{spec}/properties/files")).is_none());
    }

    #[test]
    fn inject_cel_rules_is_no_op_when_spec_path_absent() {
        // Defensive: if the CRD shape doesn't have the expected spec path
        // (shouldn't happen for real kube CRDs, but guards the `if let Some`).
        let mut crd = json!({"unrelated": "value"});
        inject_cel_rules(&mut crd);
        // No mutation happened.
        assert_eq!(crd, json!({"unrelated": "value"}));
    }
}
