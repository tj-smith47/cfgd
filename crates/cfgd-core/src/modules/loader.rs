//! Module loading and dependency resolution.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

use crate::config::parse_module;
use crate::errors::{ConfigError, ModuleError, Result};

use super::LoadedModule;

// ---------------------------------------------------------------------------
// Module loading
// ---------------------------------------------------------------------------

/// Cap on a single `module.yaml` to prevent memory exhaustion. Applies to both
/// `load_module` and the inner read in `load_modules`.
const MAX_MODULE_SIZE: u64 = 10 * 1024 * 1024; // 10 MB

/// Read a `module.yaml` after enforcing [`MAX_MODULE_SIZE`].
fn read_module_yaml_capped(module_yaml: &Path) -> Result<String> {
    if let Ok(meta) = std::fs::metadata(module_yaml)
        && meta.len() > MAX_MODULE_SIZE
    {
        return Err(ModuleError::InvalidSpec {
            name: module_yaml.display().to_string(),
            message: format!(
                "module file too large ({} bytes, max {})",
                meta.len(),
                MAX_MODULE_SIZE
            ),
        }
        .into());
    }

    std::fs::read_to_string(module_yaml).map_err(|e| {
        ConfigError::Invalid {
            message: format!("cannot read module file {}: {e}", module_yaml.display()),
        }
        .into()
    })
}

/// Load all modules from the `modules/` directory under the given config dir.
/// Returns a map of module name → LoadedModule.
pub fn load_modules(config_dir: &Path) -> Result<HashMap<String, LoadedModule>> {
    let modules_dir = config_dir.join("modules");
    if !modules_dir.is_dir() {
        return Ok(HashMap::new());
    }

    let mut modules = HashMap::new();
    let entries = std::fs::read_dir(&modules_dir).map_err(|e| ConfigError::Invalid {
        message: format!(
            "cannot read modules directory {}: {e}",
            modules_dir.display()
        ),
    })?;

    for entry in entries {
        let entry = entry.map_err(|e| ConfigError::Invalid {
            message: format!("cannot read modules directory entry: {e}"),
        })?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let module_yaml = path.join("module.yaml");
        if !module_yaml.exists() {
            continue;
        }

        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| ConfigError::Invalid {
                message: format!("invalid module directory name: {}", path.display()),
            })?
            .to_string();

        let contents = read_module_yaml_capped(&module_yaml)?;

        let doc = parse_module(&contents)?;

        if doc.metadata.name != name {
            return Err(ModuleError::InvalidSpec {
                name: name.clone(),
                message: format!(
                    "module directory '{}' does not match metadata.name '{}'",
                    name, doc.metadata.name
                ),
            }
            .into());
        }

        modules.insert(
            name.clone(),
            LoadedModule {
                name,
                spec: doc.spec,
                dir: path,
            },
        );
    }

    Ok(modules)
}

/// Load a single module from a given directory.
pub fn load_module(module_dir: &Path) -> Result<LoadedModule> {
    let module_yaml = module_dir.join("module.yaml");
    if !module_yaml.exists() {
        let name = module_dir
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| ModuleError::InvalidSpec {
                name: module_dir.display().to_string(),
                message: "invalid module directory name".into(),
            })?
            .to_string();
        return Err(ModuleError::NotFound { name }.into());
    }

    let contents = read_module_yaml_capped(&module_yaml)?;
    let doc = parse_module(&contents)?;
    let name = doc.metadata.name.clone();

    Ok(LoadedModule {
        name,
        spec: doc.spec,
        dir: module_dir.to_path_buf(),
    })
}

// ---------------------------------------------------------------------------
// Dependency resolution — topological sort with cycle detection
// ---------------------------------------------------------------------------

/// Resolve module dependencies using topological sort (Kahn's algorithm).
/// Returns module names in dependency order (leaves first).
pub fn resolve_dependency_order(
    requested: &[String],
    all_modules: &HashMap<String, LoadedModule>,
) -> Result<Vec<String>> {
    // Safety limits to prevent DoS from malicious module graphs
    const MAX_MODULES: usize = 500;
    const MAX_DEPENDENCY_DEPTH: usize = 50;

    // Collect the full set of modules we need (requested + transitive deps)
    let mut needed: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(String, usize)> = requested.iter().map(|r| (r.clone(), 0)).collect();

    while let Some((name, depth)) = queue.pop_front() {
        if needed.contains(&name) {
            continue;
        }

        if depth > MAX_DEPENDENCY_DEPTH {
            return Err(ModuleError::DependencyCycle {
                chain: vec![format!(
                    "dependency depth exceeds {} (at '{}')",
                    MAX_DEPENDENCY_DEPTH, name
                )],
            }
            .into());
        }

        if needed.len() >= MAX_MODULES {
            return Err(ModuleError::DependencyCycle {
                chain: vec![format!("total module count exceeds {} limit", MAX_MODULES)],
            }
            .into());
        }

        let module = all_modules
            .get(&name)
            .ok_or_else(|| ModuleError::NotFound { name: name.clone() })?;

        needed.insert(name.clone());

        for dep in &module.spec.depends {
            if !all_modules.contains_key(dep) {
                return Err(ModuleError::MissingDependency {
                    module: name.clone(),
                    dependency: dep.clone(),
                }
                .into());
            }
            if !needed.contains(dep) {
                queue.push_back((dep.clone(), depth + 1));
            }
        }
    }

    // Build adjacency and in-degree for the needed subset
    let mut in_degree: HashMap<String, usize> = HashMap::new();
    let mut dependents: HashMap<String, Vec<String>> = HashMap::new();

    for name in &needed {
        in_degree.entry(name.clone()).or_insert(0);
        let module = &all_modules[name];
        for dep in &module.spec.depends {
            if needed.contains(dep) {
                *in_degree.entry(name.clone()).or_insert(0) += 1;
                dependents
                    .entry(dep.clone())
                    .or_default()
                    .push(name.clone());
            }
        }
    }

    // Kahn's algorithm
    let mut queue: VecDeque<String> = in_degree
        .iter()
        .filter(|(_, deg)| **deg == 0)
        .map(|(name, _)| name.clone())
        .collect();

    // Sort the initial queue for deterministic output
    let mut sorted_initial: Vec<String> = queue.drain(..).collect();
    sorted_initial.sort();
    queue.extend(sorted_initial);

    let mut order = Vec::new();

    while let Some(name) = queue.pop_front() {
        order.push(name.clone());

        if let Some(deps) = dependents.get(&name) {
            let mut next: Vec<String> = Vec::new();
            for dep in deps {
                if let Some(deg) = in_degree.get_mut(dep) {
                    *deg -= 1;
                    if *deg == 0 {
                        next.push(dep.clone());
                    }
                }
            }
            // Sort for deterministic output
            next.sort();
            queue.extend(next);
        }
    }

    if order.len() != needed.len() {
        // Cycle detected — find the cycle members (use HashSet for O(1) lookup)
        let ordered: HashSet<&str> = order.iter().map(|s| s.as_str()).collect();
        let in_cycle: Vec<String> = needed
            .into_iter()
            .filter(|n| !ordered.contains(n.as_str()))
            .collect();
        return Err(ModuleError::DependencyCycle { chain: in_cycle }.into());
    }

    Ok(order)
}
