//! Guard: the operator's ClusterRole rules must be identical in the Helm chart
//! (`chart/cfgd/templates/rbac.yaml`) and the OLM CSV
//! (`ecosystem/olm/manifests/cfgd-operator.clusterserviceversion.yaml`).
//!
//! The two are hand-authored in different shapes — a Helm `ClusterRole` template
//! vs the CSV's `spec.install.spec.clusterPermissions` — with no shared source,
//! so they drift. They have before: an OLM-installed operator silently lacked
//! `finalizers/update`, `events.k8s.io`, and namespace `watch` that the Helm
//! install granted, while carrying `pods` / `webhookconfigurations` grants the
//! operator never uses. This test fails the build the moment they diverge again.

use std::collections::BTreeSet;
use std::path::PathBuf;

/// A single RBAC PolicyRule reduced to a canonical, order-insensitive form:
/// (sorted apiGroups, sorted resources, sorted verbs).
type NormalizedRule = (Vec<String>, Vec<String>, Vec<String>);

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = <root>/crates/cfgd-operator
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn normalize_rules(rules: &serde_yaml::Value) -> BTreeSet<NormalizedRule> {
    let seq = rules.as_sequence().expect("rules must be a YAML sequence");
    seq.iter()
        .map(|rule| {
            let field = |key: &str| -> Vec<String> {
                let mut values: Vec<String> = rule
                    .get(key)
                    .and_then(serde_yaml::Value::as_sequence)
                    .map(|s| {
                        s.iter()
                            .filter_map(|e| e.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                values.sort();
                values
            };
            (field("apiGroups"), field("resources"), field("verbs"))
        })
        .collect()
}

/// Extract the first column-0 `rules:` block from the Helm ClusterRole template
/// up to the `---` document separator. The rule entries are plain YAML (no
/// `{{ }}` interpolation), so the slice parses on its own.
fn chart_rules() -> BTreeSet<NormalizedRule> {
    let path = repo_root().join("chart/cfgd/templates/rbac.yaml");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut block = String::new();
    let mut in_rules = false;
    for line in text.lines() {
        if line.trim_end() == "rules:" {
            in_rules = true;
        }
        if in_rules {
            if line.trim_end() == "---" {
                break;
            }
            block.push_str(line);
            block.push('\n');
        }
    }
    assert!(
        in_rules,
        "no column-0 `rules:` block found in chart rbac.yaml"
    );
    let doc: serde_yaml::Value =
        serde_yaml::from_str(&block).expect("chart rules block must parse as YAML");
    normalize_rules(&doc["rules"])
}

fn csv_rules() -> BTreeSet<NormalizedRule> {
    let path = repo_root().join("ecosystem/olm/manifests/cfgd-operator.clusterserviceversion.yaml");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let csv: serde_yaml::Value = serde_yaml::from_str(&text).expect("CSV must parse as YAML");
    normalize_rules(&csv["spec"]["install"]["spec"]["clusterPermissions"][0]["rules"])
}

#[test]
fn operator_rbac_identical_in_chart_and_olm_csv() {
    let chart = chart_rules();
    let csv = csv_rules();
    let only_chart: Vec<_> = chart.difference(&csv).collect();
    let only_csv: Vec<_> = csv.difference(&chart).collect();
    assert!(
        only_chart.is_empty() && only_csv.is_empty(),
        "operator RBAC drift between Helm chart and OLM CSV.\n  only in chart: {only_chart:#?}\n  only in CSV: {only_csv:#?}"
    );
}
