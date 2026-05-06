use crate::config;

/// Deep merge two YAML values. Mappings are merged recursively; all other
/// types are replaced by the overlay value.
pub fn deep_merge_yaml(base: &mut serde_yaml::Value, overlay: &serde_yaml::Value) {
    match (base, overlay) {
        (serde_yaml::Value::Mapping(base_map), serde_yaml::Value::Mapping(overlay_map)) => {
            for (key, value) in overlay_map {
                if let Some(base_value) = base_map.get_mut(key) {
                    deep_merge_yaml(base_value, value);
                } else {
                    base_map.insert(key.clone(), value.clone());
                }
            }
        }
        (base, overlay) => {
            *base = overlay.clone();
        }
    }
}

/// Extend a `Vec<String>` with items from `source`, skipping duplicates.
pub fn union_extend(target: &mut Vec<String>, source: &[String]) {
    let mut existing: std::collections::HashSet<String> = target.iter().cloned().collect();
    for item in source {
        if existing.insert(item.clone()) {
            target.push(item.clone());
        }
    }
}

/// Merge env vars by name: later entries override earlier ones with the same name.
/// Used by config layer merging, composition, and reconciler module merge.
pub fn merge_env(base: &mut Vec<config::EnvVar>, updates: &[config::EnvVar]) {
    let mut index: std::collections::HashMap<String, usize> = base
        .iter()
        .enumerate()
        .map(|(i, e)| (e.name.clone(), i))
        .collect();
    for ev in updates {
        if let Some(&pos) = index.get(&ev.name) {
            base[pos] = ev.clone();
        } else {
            index.insert(ev.name.clone(), base.len());
            base.push(ev.clone());
        }
    }
}

/// Merge shell aliases by name: later entries override earlier ones with the same name.
/// Same semantics as `merge_env`.
pub fn merge_aliases(base: &mut Vec<config::ShellAlias>, updates: &[config::ShellAlias]) {
    let mut index: std::collections::HashMap<String, usize> = base
        .iter()
        .enumerate()
        .map(|(i, a)| (a.name.clone(), i))
        .collect();
    for alias in updates {
        if let Some(&pos) = index.get(&alias.name) {
            base[pos] = alias.clone();
        } else {
            index.insert(alias.name.clone(), base.len());
            base.push(alias.clone());
        }
    }
}

/// Split a list of values into adds and removes.
///
/// Values starting with `-` are treated as removals (the leading `-` is stripped).
/// All other values are adds. This powers the unified `--thing` CLI flags where
/// `--thing foo` adds and `--thing -foo` removes.
pub fn split_add_remove(values: &[String]) -> (Vec<String>, Vec<String>) {
    let mut adds = Vec::new();
    let mut removes = Vec::new();
    for v in values {
        if let Some(stripped) = v.strip_prefix('-') {
            removes.push(stripped.to_string());
        } else {
            adds.push(v.clone());
        }
    }
    (adds, removes)
}
