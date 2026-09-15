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

/// Merge env vars by name: later entries override earlier ones with the same
/// name — the DOCUMENT-edit semantic, which is why the CLI setters call it:
/// `profile update --env PATH=x` replaces the file's `PATH` declaration rather
/// than appending to it.
///
/// A LAYER fold takes [`fold_env_layer`] instead, where `PATH` concatenates.
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

/// Fold `overlay` onto `base` as one LAYER of a machine's desired state.
///
/// Every name but `PATH` is last-writer-wins, exactly as [`merge_env`]. `PATH`
/// is the one variable whose value is a LIST: a profile declaring the common
/// entries and a module (or a `platforms:`-gated sibling entry) declaring more
/// both apply, and replacing one with the other silently discards directories
/// the user asked for. So the surviving declarations concatenate.
///
/// Each declaration splits on `separator` into the entries BEFORE its ambient
/// `PATH` reference and the entries AFTER it, and the fold renders
/// `before…:$PATH:after…`. The two buckets fill in OPPOSITE layer orders,
/// because that is what the shell chaining they stand in for does: run
/// `export PATH=<base>:$PATH` and then `export PATH=<overlay>:$PATH` and the
/// later prepend lands in FRONT, while `export PATH=$PATH:<base>` followed by
/// `export PATH=$PATH:<overlay>` leaves the earlier append in front. So the
/// prepend bucket takes the overlay's entries ahead of the base's and the
/// append bucket keeps base first. The ambient reference is written once, in
/// the spelling of the first declaration that named one, and is absent only
/// when no declaration named one (that declaration is taken at its word,
/// exactly as `fold_path_line` takes it). Duplicates drop on
/// [`crate::normalize_path_entry`], the first occurrence in the RENDERED order
/// winning, because `$HOME/.cargo/bin` and `/home/x/.cargo/bin` are one
/// directory written two ways.
///
/// The result is at most ONE `PATH` entry per merged env, which is what lets
/// `fold_path_line` keep finding the declaration with a single lookup.
pub fn fold_env_layer(base: &mut Vec<config::EnvVar>, overlay: &[config::EnvVar], separator: char) {
    let (path_overlay, plain): (Vec<&config::EnvVar>, Vec<&config::EnvVar>) =
        overlay.iter().partition(|e| e.name == PATH_VAR);
    let plain: Vec<config::EnvVar> = plain.into_iter().cloned().collect();
    merge_env(base, &plain);
    if path_overlay.is_empty() {
        return;
    }

    let home = crate::expand_tilde(std::path::Path::new("~"));
    let mut inherited: Option<String> = None;

    let split_at_reference = |value: &str, inherited: &mut Option<String>| {
        let (mut before, mut after): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
        let mut past_ref = false;
        for segment in value.split(separator) {
            if segment.is_empty() {
                continue;
            }
            if crate::is_inherited_path_ref(segment) {
                past_ref = true;
                inherited.get_or_insert_with(|| segment.trim().to_string());
                continue;
            }
            if past_ref { &mut after } else { &mut before }.push(segment.trim().to_string());
        }
        (before, after)
    };

    let existing = base.iter().position(|e| e.name == PATH_VAR);
    // The base is read first so the ambient reference keeps the spelling of the
    // earliest declaration that named one.
    let base_layer = existing.map(|pos| split_at_reference(&base[pos].value, &mut inherited));
    let overlay_layers: Vec<(Vec<String>, Vec<String>)> = path_overlay
        .iter()
        .map(|entry| split_at_reference(&entry.value, &mut inherited))
        .collect();

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut parts: Vec<String> = Vec::new();
    let take =
        |parts: &mut Vec<String>, seen: &mut std::collections::HashSet<String>, segment: &str| {
            if seen.insert(crate::normalize_path_entry(segment, &home)) {
                parts.push(segment.to_string());
            }
        };
    // A prepend puts the overlay's directories ahead of the base's, the way the
    // later `PATH=<dir>:$PATH` of two chained declarations wins the lookup;
    // within one layer the declarations keep the order they were written in.
    for (before, _) in overlay_layers.iter().chain(base_layer.iter()) {
        for segment in before {
            take(&mut parts, &mut seen, segment);
        }
    }
    if let Some(reference) = inherited {
        parts.push(reference);
    }
    for (_, after) in base_layer.iter().chain(overlay_layers.iter()) {
        for segment in after {
            take(&mut parts, &mut seen, segment);
        }
    }
    // A gated `PATH` entry that survives is part of THIS host's state and its
    // tags have already done their work; the folded entry carries none, so no
    // later reader re-applies a gate to a value several declarations produced.
    let folded = config::EnvVar {
        name: PATH_VAR.to_string(),
        value: parts.join(&separator.to_string()),
        platforms: Vec::new(),
    };
    match existing {
        Some(pos) => base[pos] = folded,
        None => base.push(folded),
    }
}

/// The one variable whose declarations concatenate rather than replace.
const PATH_VAR: &str = "PATH";

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

/// Merge backup units by name: later entries override earlier ones with the same name.
/// Same semantics as `merge_env`.
pub fn merge_backups(base: &mut Vec<config::BackupSpec>, updates: &[config::BackupSpec]) {
    let mut index: std::collections::HashMap<String, usize> = base
        .iter()
        .enumerate()
        .map(|(i, b)| (b.name.clone(), i))
        .collect();
    for backup in updates {
        if let Some(&pos) = index.get(&backup.name) {
            base[pos] = backup.clone();
        } else {
            index.insert(backup.name.clone(), base.len());
            base.push(backup.clone());
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
