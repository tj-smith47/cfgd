//! CRD YAML generator — writes serialized CRDs to stdout for the Helm chart.
//!
//! # Hard-Rule #1 exemption
//!
//! This is a standalone build-tool binary whose entire contract is
//! "emit well-formed YAML on stdout so the caller can `> file.yaml`".
//! The `output::Printer` abstraction is a structured terminal interface
//! (headers, spinners, styling) and cannot produce raw YAML on stdout
//! without corrupting the output. The direct `print!` below is therefore
//! the correct tool, documented here so future audits / reviewers don't
//! re-flag it as a Hard-Rule #1 violation.
//!
//! This file is the ONLY `print!`/`println!` use outside of the
//! `output` module in the cfgd workspace.

use kube::CustomResourceExt;

use cfgd_operator::crds::{ClusterConfigPolicy, ConfigPolicy, DriftAlert, MachineConfig, Module};

fn main() {
    let mut mc_crd =
        serde_json::to_value(MachineConfig::crd()).expect("serialize MachineConfig CRD");
    inject_cel_rules(&mut mc_crd);
    inject_smd_annotations(&mut mc_crd);
    let mc = serde_yaml::to_string(&mc_crd).expect("MachineConfig CRD to YAML");

    let mut cp_crd = serde_json::to_value(ConfigPolicy::crd()).expect("serialize ConfigPolicy CRD");
    inject_smd_annotations(&mut cp_crd);
    let cp = serde_yaml::to_string(&cp_crd).expect("ConfigPolicy CRD to YAML");

    let mut da_crd = serde_json::to_value(DriftAlert::crd()).expect("serialize DriftAlert CRD");
    inject_smd_annotations(&mut da_crd);
    let da = serde_yaml::to_string(&da_crd).expect("DriftAlert CRD to YAML");

    let mut ccp_crd = serde_json::to_value(ClusterConfigPolicy::crd())
        .expect("serialize ClusterConfigPolicy CRD");
    inject_smd_annotations(&mut ccp_crd);
    let ccp = serde_yaml::to_string(&ccp_crd).expect("ClusterConfigPolicy CRD to YAML");

    let mut mod_crd = serde_json::to_value(Module::crd()).expect("serialize Module CRD");
    inject_smd_annotations(&mut mod_crd);
    let modl = serde_yaml::to_string(&mod_crd).expect("Module CRD to YAML");

    print!("{mc}---\n{cp}---\n{da}---\n{ccp}---\n{modl}");
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
