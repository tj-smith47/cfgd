use kube::CustomResourceExt;

use cfgd_operator::crds::{ConfigPolicy, DriftAlert, MachineConfig};

fn main() {
    let mut mc_crd =
        serde_json::to_value(MachineConfig::crd()).expect("serialize MachineConfig CRD");
    inject_cel_rules(&mut mc_crd);
    let mc = serde_yaml::to_string(&mc_crd).expect("MachineConfig CRD to YAML");

    let cp = serde_yaml::to_string(&ConfigPolicy::crd()).expect("serialize ConfigPolicy CRD");
    let da = serde_yaml::to_string(&DriftAlert::crd()).expect("serialize DriftAlert CRD");
    print!("{mc}---\n{cp}---\n{da}");
}

fn inject_cel_rules(crd: &mut serde_json::Value) {
    if let Some(spec) =
        crd.pointer_mut("/spec/versions/0/schema/openAPIV3Schema/properties/spec")
    {
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
