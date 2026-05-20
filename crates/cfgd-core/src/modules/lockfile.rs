//! Module lockfile — tracking remote modules with integrity hashes,
//! and module-spec diffing for sync output.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::config::{ModuleLockEntry, ModuleLockfile};
use crate::errors::{ConfigError, ModuleError, Result};

use super::LoadedModule;
use super::git::{GitSource, fetch_git_source, git_cache_dir, parse_git_source, resolve_subdir};
use super::loader::{load_module, load_modules};

/// Load the module lockfile from `<config_dir>/modules.lock`.
/// Returns an empty lockfile if the file does not exist.
pub fn load_lockfile(config_dir: &Path) -> Result<ModuleLockfile> {
    let lockfile_path = config_dir.join("modules.lock");
    if !lockfile_path.exists() {
        return Ok(ModuleLockfile::default());
    }
    let contents = std::fs::read_to_string(&lockfile_path).map_err(|e| ConfigError::Invalid {
        message: format!("cannot read lockfile {}: {e}", lockfile_path.display()),
    })?;
    let lockfile: ModuleLockfile = serde_yaml::from_str(&contents).map_err(ConfigError::from)?;
    Ok(lockfile)
}

/// Save the module lockfile to `<config_dir>/modules.lock`.
/// Uses `atomic_write_str` (temp file + rename) to prevent corruption.
pub fn save_lockfile(config_dir: &Path, lockfile: &ModuleLockfile) -> Result<()> {
    let lockfile_path = config_dir.join("modules.lock");
    let contents = serde_yaml::to_string(lockfile).map_err(ConfigError::from)?;
    crate::atomic_write_str(&lockfile_path, &contents).map_err(|e| ConfigError::Invalid {
        message: format!("cannot write lockfile {}: {e}", lockfile_path.display()),
    })?;
    Ok(())
}

/// Compute SHA-256 integrity hash of a module directory's contents.
/// Hashes file paths (relative to module dir) and their contents, sorted for determinism.
pub fn hash_module_contents(module_dir: &Path) -> Result<String> {
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    collect_files_for_hash(module_dir, module_dir, &mut entries)?;
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut hasher_input = Vec::new();
    for (rel_path, content) in &entries {
        hasher_input.extend_from_slice(rel_path.as_bytes());
        hasher_input.push(0);
        hasher_input.extend_from_slice(content);
        hasher_input.push(0);
    }

    Ok(crate::sha256_digest(&hasher_input))
}

fn collect_files_for_hash(
    base: &Path,
    current: &Path,
    entries: &mut Vec<(String, Vec<u8>)>,
) -> Result<()> {
    if !current.is_dir() {
        return Ok(());
    }
    let dir_entries = std::fs::read_dir(current)?;

    for entry in dir_entries {
        let entry = entry?;
        let path = entry.path();
        // Skip .git directory
        if path.file_name().is_some_and(|n| n == ".git") {
            continue;
        }
        // Skip symlinks — only hash real files to avoid infinite recursion
        // and to avoid hashing files outside the module tree
        let meta = std::fs::symlink_metadata(&path)?;
        if meta.is_symlink() {
            continue;
        }
        if meta.is_dir() {
            collect_files_for_hash(base, &path, entries)?;
        } else {
            let rel = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
            let content = std::fs::read(&path)?;
            entries.push((rel, content));
        }
    }
    Ok(())
}

/// Verify the integrity of a locked remote module against its lockfile entry.
pub fn verify_lockfile_integrity(lock_entry: &ModuleLockEntry, cache_base: &Path) -> Result<()> {
    let git_src = parse_git_source(&lock_entry.url)?;
    let local_path = resolve_subdir(
        git_cache_dir(cache_base, &git_src.repo_url),
        &lock_entry.subdir,
        &lock_entry.name,
        &lock_entry.url,
    )?;

    if !local_path.exists() {
        return Err(ModuleError::GitFetchFailed {
            module: lock_entry.name.clone(),
            url: lock_entry.url.clone(),
            message: "cached module directory does not exist — run 'cfgd module update'".into(),
        }
        .into());
    }

    let actual_integrity = hash_module_contents(&local_path)?;
    if actual_integrity != lock_entry.integrity {
        return Err(ModuleError::IntegrityMismatch {
            name: lock_entry.name.clone(),
            expected: lock_entry.integrity.clone(),
            actual: actual_integrity,
        }
        .into());
    }

    Ok(())
}

/// Load remote modules from the lockfile, fetching if needed, and merge
/// them into the given modules map.
pub fn load_locked_modules(
    config_dir: &Path,
    cache_base: &Path,
    modules: &mut HashMap<String, LoadedModule>,
    printer: &crate::output::Printer,
) -> Result<()> {
    let lockfile = load_lockfile(config_dir)?;

    for entry in &lockfile.modules {
        // Skip if a local module with the same name already exists (local wins)
        if modules.contains_key(&entry.name) {
            continue;
        }

        let git_src = parse_git_source(&entry.url)?;

        // Build a GitSource with the pinned ref
        let pinned_src = GitSource {
            repo_url: git_src.repo_url.clone(),
            tag: Some(entry.pinned_ref.clone()),
            git_ref: None,
            subdir: entry.subdir.clone(),
        };

        // Fetch to cache (no-op if already present at correct ref)
        let local_path = fetch_git_source(&pinned_src, cache_base, &entry.name, printer)?;

        // Verify integrity
        verify_lockfile_integrity(entry, cache_base)?;

        // Load the module
        let module = load_module(&local_path)?;
        modules.insert(entry.name.clone(), module);
    }

    Ok(())
}

/// Load all modules: local modules from disk + remote locked modules.
pub fn load_all_modules(
    config_dir: &Path,
    cache_base: &Path,
    printer: &crate::output::Printer,
) -> Result<HashMap<String, LoadedModule>> {
    let mut modules = load_modules(config_dir)?;
    load_locked_modules(config_dir, cache_base, &mut modules, printer)?;
    Ok(modules)
}

/// Diff two module specs, returning a human-readable summary of changes.
pub fn diff_module_specs(old: &LoadedModule, new: &LoadedModule) -> Vec<String> {
    let mut changes = Vec::new();

    // Dependencies
    let old_deps: HashSet<&str> = old.spec.depends.iter().map(|s| s.as_str()).collect();
    let new_deps: HashSet<&str> = new.spec.depends.iter().map(|s| s.as_str()).collect();
    for dep in new_deps.difference(&old_deps) {
        changes.push(format!("+ dependency: {dep}"));
    }
    for dep in old_deps.difference(&new_deps) {
        changes.push(format!("- dependency: {dep}"));
    }

    // Packages
    let old_pkgs: HashSet<&str> = old.spec.packages.iter().map(|p| p.name.as_str()).collect();
    let new_pkgs: HashSet<&str> = new.spec.packages.iter().map(|p| p.name.as_str()).collect();
    for pkg in new_pkgs.difference(&old_pkgs) {
        changes.push(format!("+ package: {pkg}"));
    }
    for pkg in old_pkgs.difference(&new_pkgs) {
        changes.push(format!("- package: {pkg}"));
    }

    // Check for version constraint changes on existing packages
    for new_pkg in &new.spec.packages {
        if let Some(old_pkg) = old.spec.packages.iter().find(|p| p.name == new_pkg.name)
            && old_pkg.min_version != new_pkg.min_version
        {
            changes.push(format!(
                "~ package '{}': minVersion {} -> {}",
                new_pkg.name,
                old_pkg.min_version.as_deref().unwrap_or("(none)"),
                new_pkg.min_version.as_deref().unwrap_or("(none)")
            ));
        }
    }

    // Files
    let old_files: HashSet<&str> = old.spec.files.iter().map(|f| f.target.as_str()).collect();
    let new_files: HashSet<&str> = new.spec.files.iter().map(|f| f.target.as_str()).collect();
    for file in new_files.difference(&old_files) {
        changes.push(format!("+ file target: {file}"));
    }
    for file in old_files.difference(&new_files) {
        changes.push(format!("- file target: {file}"));
    }

    // Scripts
    let old_scripts: Vec<&str> = old
        .spec
        .scripts
        .as_ref()
        .map(|s| s.post_apply.iter().map(|e| e.run_str()).collect())
        .unwrap_or_default();
    let new_scripts: Vec<&str> = new
        .spec
        .scripts
        .as_ref()
        .map(|s| s.post_apply.iter().map(|e| e.run_str()).collect())
        .unwrap_or_default();
    let old_script_set: HashSet<&str> = old_scripts.into_iter().collect();
    let new_script_set: HashSet<&str> = new_scripts.into_iter().collect();
    for script in new_script_set.difference(&old_script_set) {
        changes.push(format!("+ postApply script: {script}"));
    }
    for script in old_script_set.difference(&new_script_set) {
        changes.push(format!("- postApply script: {script}"));
    }

    if changes.is_empty() {
        changes.push("(no spec changes)".to_string());
    }

    changes
}
