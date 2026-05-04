// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(super) fn find_toml_value(table: &toml::Table, key: &str) -> Option<String> {
    // First try dot-separated path lookup
    if key.contains('.') {
        let parts: Vec<&str> = key.rsplitn(2, '.').collect();
        let (leaf, path) = (parts[0], parts[1]);
        let mut current = table;
        let mut found = true;
        for segment in path.split('.') {
            match current.get(segment).and_then(|v| v.as_table()) {
                Some(t) => current = t,
                None => {
                    found = false;
                    break;
                }
            }
        }
        if found && let Some(val) = current.get(leaf) {
            return Some(toml_value_to_string(val));
        }
    }

    // Fall back to direct key lookup at root level
    if let Some(val) = table.get(key) {
        return Some(toml_value_to_string(val));
    }

    // Fall back to recursive search for backward compatibility
    for (_, val) in table {
        if let toml::Value::Table(nested) = val
            && let Some(found) = find_toml_value(nested, key)
        {
            return Some(found);
        }
    }

    None
}

pub(super) fn toml_value_to_string(value: &toml::Value) -> String {
    match value {
        toml::Value::Boolean(b) => b.to_string(),
        toml::Value::Integer(n) => n.to_string(),
        toml::Value::Float(n) => n.to_string(),
        toml::Value::String(s) => s.clone(),
        _ => format!("{}", value),
    }
}

pub(super) fn set_toml_value(table: &mut toml::Table, key: &str, value: &serde_yaml::Value) {
    let toml_val = yaml_to_toml_value(value);

    if !key.contains('.') {
        table.insert(key.to_string(), toml_val);
        return;
    }

    let parts: Vec<&str> = key.rsplitn(2, '.').collect();
    let (leaf, path) = (parts[0], parts[1]);

    let mut current = table;
    for segment in path.split('.') {
        let entry = current
            .entry(segment.to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if !entry.is_table() {
            *entry = toml::Value::Table(toml::Table::new());
        }
        // Safe: we just set it to a Table two lines above if it wasn't one
        current = match entry.as_table_mut() {
            Some(t) => t,
            None => return, // unreachable after the assignment above
        };
    }
    current.insert(leaf.to_string(), toml_val);
}

pub(super) fn yaml_to_toml_value(value: &serde_yaml::Value) -> toml::Value {
    match value {
        serde_yaml::Value::Bool(b) => toml::Value::Boolean(*b),
        serde_yaml::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                toml::Value::Integer(i)
            } else if let Some(f) = n.as_f64() {
                toml::Value::Float(f)
            } else {
                toml::Value::String(n.to_string())
            }
        }
        serde_yaml::Value::String(s) => toml::Value::String(s.clone()),
        serde_yaml::Value::Mapping(m) => {
            let mut table = toml::Table::new();
            for (k, v) in m {
                if let Some(key) = k.as_str() {
                    table.insert(key.to_string(), yaml_to_toml_value(v));
                }
            }
            toml::Value::Table(table)
        }
        serde_yaml::Value::Sequence(s) => {
            let arr: Vec<toml::Value> = s.iter().map(yaml_to_toml_value).collect();
            toml::Value::Array(arr)
        }
        _ => toml::Value::String(String::new()),
    }
}

/// Compare two JSON strings for semantic equality.
/// Returns true if both parse to equal `serde_json::Value`s, or if both
/// raw strings are equal after trimming (fallback for non-JSON input).
pub(super) fn json_equal(a: &str, b: &str) -> bool {
    match (
        serde_json::from_str::<serde_json::Value>(a),
        serde_json::from_str::<serde_json::Value>(b),
    ) {
        (Ok(va), Ok(vb)) => va == vb,
        _ => a.trim() == b.trim(),
    }
}
