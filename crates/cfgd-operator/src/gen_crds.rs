//! CRD YAML generation for the Helm chart.
//!
//! Sources the five CRD spec types from `cfgd-crd` (via the operator re-export)
//! and renders kube's `CustomResourceExt::crd()` output to YAML, injecting the
//! `x-kubernetes-list-type` / CEL structural-merge annotations that schemars
//! cannot express. [`render_all`] is the testable library entry point; the
//! `cfgd-gen-crds` binary wraps it for stdout / file-tree emission.

use kube::CustomResourceExt;
use thiserror::Error;

use crate::crds::{ClusterConfigPolicy, ConfigPolicy, DriftAlert, MachineConfig, Module};

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
    let name = crd
        .pointer("/metadata/name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let yaml = serde_yaml::to_string(&crd)?;
    Ok(RenderedCrd { name, yaml })
}

/// Render all five CRDs, each as a [`RenderedCrd`], in the chart's canonical
/// order (MachineConfig, ConfigPolicy, DriftAlert, ClusterConfigPolicy, Module).
pub fn render_each() -> Result<Vec<RenderedCrd>, GenCrdsError> {
    Ok(vec![
        // MachineConfig is the only kind carrying the hostname / files CEL rules.
        render_crd(serde_json::to_value(MachineConfig::crd())?, true)?,
        render_crd(serde_json::to_value(ConfigPolicy::crd())?, false)?,
        render_crd(serde_json::to_value(DriftAlert::crd())?, false)?,
        render_crd(serde_json::to_value(ClusterConfigPolicy::crd())?, false)?,
        render_crd(serde_json::to_value(Module::crd())?, false)?,
    ])
}

/// Render all five CRDs into a single `---\n`-joined YAML document — the exact
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

    // files list: merge by map key — "path" for MachineConfig, "source" for Module
    if let Some(files) = crd.pointer_mut(&format!("{spec_base}/spec/properties/files")) {
        files["x-kubernetes-list-type"] = serde_json::json!("map");
        // Determine map key from the items schema: Module files have "source"+"target",
        // MachineConfig files have "path"+"content"+"source"+"mode".
        let has_path_property = files.pointer("/items/properties/path").is_some();
        if has_path_property {
            files["x-kubernetes-list-map-keys"] = serde_json::json!(["path"]);
        } else {
            files["x-kubernetes-list-map-keys"] = serde_json::json!(["source"]);
        }
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
    use super::{inject_cel_rules, inject_smd_annotations, render_all};
    use serde_json::{Value, json};

    #[test]
    fn render_all_covers_all_five_crds() {
        let yaml = render_all().expect("render CRDs");
        for k in [
            "machineconfigs",
            "configpolicies",
            "clusterconfigpolicies",
            "driftalerts",
            "modules",
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
        // Five CRDs joined by `---\n` => exactly four separators.
        assert_eq!(yaml.matches("---\n").count(), 4);
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
                                        "conditions": {"items": {}}
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
    fn inject_smd_annotations_files_list_keys_by_source_when_items_lack_path_property() {
        // Module shape: items.properties has "source"+"target" but not "path".
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
            Some(json!(["source"]))
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
