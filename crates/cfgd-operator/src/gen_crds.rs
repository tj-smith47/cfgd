use kube::CustomResourceExt;

use cfgd_operator::crds::{ClusterConfigPolicy, ConfigPolicy, DriftAlert, MachineConfig};

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

    print!("{mc}---\n{cp}---\n{da}---\n{ccp}");
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

    // files list: merge by "path" key (MachineConfig only)
    if let Some(files) = crd.pointer_mut(&format!("{spec_base}/spec/properties/files")) {
        files["x-kubernetes-list-type"] = serde_json::json!("map");
        files["x-kubernetes-list-map-keys"] = serde_json::json!(["path"]);
    }

    // driftDetails list: merge by "field" key (DriftAlert only)
    if let Some(details) = crd.pointer_mut(&format!("{spec_base}/spec/properties/driftDetails")) {
        details["x-kubernetes-list-type"] = serde_json::json!("map");
        details["x-kubernetes-list-map-keys"] = serde_json::json!(["field"]);
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
