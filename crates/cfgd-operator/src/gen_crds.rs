use kube::CustomResourceExt;

use cfgd_operator::crds::{ConfigPolicy, DriftAlert, MachineConfig};

fn main() {
    let mc = serde_yaml::to_string(&MachineConfig::crd()).expect("serialize MachineConfig CRD");
    let cp = serde_yaml::to_string(&ConfigPolicy::crd()).expect("serialize ConfigPolicy CRD");
    let da = serde_yaml::to_string(&DriftAlert::crd()).expect("serialize DriftAlert CRD");
    print!("{mc}---\n{cp}---\n{da}");
}
