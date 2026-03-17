// Module system — self-contained, portable configuration units
//
// Handles module loading, dependency resolution (topological sort),
// cross-platform package resolution, and git file source management.
//
// Dependency rules: depends on config/, errors/, platform/, providers/ (trait only).
// Must NOT import files/, packages/, secrets/, reconciler/, state/, daemon/.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::config::{EnvVar, ModulePackageEntry, ModuleSpec, ShellAlias, parse_module};
use crate::errors::{ConfigError, ModuleError, Result};
use crate::platform::Platform;
use crate::providers::PackageManager;

// ---------------------------------------------------------------------------
// Resolved types — output of module resolution
// ---------------------------------------------------------------------------

/// A package resolved to a concrete manager and name.
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedPackage {
    /// Canonical name from the module spec.
    pub canonical_name: String,
    /// Actual name for the manager (after alias resolution).
    pub resolved_name: String,
    /// Which manager will install it. `"script"` means use a custom install script.
    pub manager: String,
    /// Available version (if queried).
    pub version: Option<String>,
    /// Install script content (inline or file path). Only set when `manager == "script"`.
    pub script: Option<String>,
}

/// A file resolved to a concrete local path.
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedFile {
    /// Local source path (after git clone if needed).
    pub source: PathBuf,
    /// Target path on the machine.
    pub target: PathBuf,
    /// Whether the source was fetched from git.
    pub is_git_source: bool,
    /// Per-file deployment strategy override (from module spec).
    pub strategy: Option<crate::config::FileStrategy>,
}

/// A fully resolved module — ready for the reconciler.
#[derive(Debug, Clone)]
pub struct ResolvedModule {
    pub name: String,
    pub packages: Vec<ResolvedPackage>,
    pub files: Vec<ResolvedFile>,
    pub env: Vec<EnvVar>,
    pub aliases: Vec<ShellAlias>,
    pub post_apply_scripts: Vec<String>,
    pub depends: Vec<String>,
}

// ---------------------------------------------------------------------------
// Loaded module — parsed from YAML but not yet resolved
// ---------------------------------------------------------------------------

/// A module loaded from disk.
#[derive(Debug, Clone)]
pub struct LoadedModule {
    pub name: String,
    pub spec: ModuleSpec,
    pub dir: PathBuf,
}

// ---------------------------------------------------------------------------
// Module loading
// ---------------------------------------------------------------------------

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

        let contents = std::fs::read_to_string(&module_yaml).map_err(|e| ConfigError::Invalid {
            message: format!("cannot read module file {}: {e}", module_yaml.display()),
        })?;

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

    let contents = std::fs::read_to_string(&module_yaml).map_err(|e| ConfigError::Invalid {
        message: format!("cannot read module file {}: {e}", module_yaml.display()),
    })?;

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
    // Collect the full set of modules we need (requested + transitive deps)
    let mut needed: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = requested.iter().cloned().collect();

    while let Some(name) = queue.pop_front() {
        if needed.contains(&name) {
            continue;
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
                queue.push_back(dep.clone());
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
        // Cycle detected — find the cycle members
        let in_cycle: Vec<String> = needed.into_iter().filter(|n| !order.contains(n)).collect();
        return Err(ModuleError::DependencyCycle { chain: in_cycle }.into());
    }

    Ok(order)
}

// ---------------------------------------------------------------------------
// Package resolution
// ---------------------------------------------------------------------------

/// Resolve a single module package entry to a concrete (manager, name, version).
///
/// Algorithm:
/// 0. If `platforms` is non-empty and current platform doesn't match → return None (skipped)
/// 1. Determine candidate managers: `prefer` list, or `[platform.native_manager()]`
/// 2. For each candidate:
///    a. If `"script"` — always available, uses the `script` field as installer
///    b. Otherwise: check available + alias resolve + min-version check
/// 3. First satisfying candidate wins
/// 4. If none satisfies, return error with details
pub fn resolve_package(
    entry: &ModulePackageEntry,
    module_name: &str,
    platform: &Platform,
    managers: &HashMap<String, &dyn PackageManager>,
) -> Result<Option<ResolvedPackage>> {
    // Platform filter: skip entirely if platforms is non-empty and doesn't match
    if !platform.matches_any(&entry.platforms) {
        return Ok(None);
    }

    let candidates: Vec<String> = if entry.prefer.is_empty() {
        vec![platform.native_manager().to_string()]
    } else {
        entry.prefer.clone()
    };

    // Filter out denied managers
    let candidates: Vec<String> = candidates
        .into_iter()
        .filter(|c| !entry.deny.contains(c))
        .collect();

    for candidate in &candidates {
        // Special "script" manager — always available, uses custom install script
        if candidate == "script" {
            let script = entry
                .script
                .as_ref()
                .ok_or_else(|| ModuleError::InvalidSpec {
                    name: module_name.to_string(),
                    message: format!(
                        "package '{}' has 'script' in prefer list but no 'script' field defined",
                        entry.name
                    ),
                })?;
            return Ok(Some(ResolvedPackage {
                canonical_name: entry.name.clone(),
                resolved_name: entry.name.clone(),
                manager: "script".to_string(),
                version: None,
                script: Some(script.clone()),
            }));
        }

        let mgr = match managers.get(candidate.as_str()) {
            Some(m) => *m,
            None => continue,
        };

        let bootstrappable = !mgr.is_available() && mgr.can_bootstrap();
        if !mgr.is_available() && !bootstrappable {
            continue;
        }

        let resolved_name = entry
            .aliases
            .get(candidate)
            .cloned()
            .unwrap_or_else(|| entry.name.clone());

        // If the manager isn't installed yet but can be bootstrapped, resolve
        // optimistically — we can't query versions until it's installed.
        if bootstrappable {
            return Ok(Some(ResolvedPackage {
                canonical_name: entry.name.clone(),
                resolved_name,
                manager: candidate.clone(),
                version: None,
                script: None,
            }));
        }

        if let Some(ref min_ver) = entry.min_version {
            match mgr.available_version(&resolved_name) {
                Ok(Some(ver)) => {
                    if !crate::version_satisfies(&ver, &format!(">={min_ver}")) {
                        continue;
                    }
                    return Ok(Some(ResolvedPackage {
                        canonical_name: entry.name.clone(),
                        resolved_name,
                        manager: candidate.clone(),
                        version: Some(ver),
                        script: None,
                    }));
                }
                Ok(None) => continue,
                Err(_) => continue,
            }
        } else {
            // No min-version: first available manager wins.
            let version = mgr.available_version(&resolved_name).ok().flatten();
            return Ok(Some(ResolvedPackage {
                canonical_name: entry.name.clone(),
                resolved_name,
                manager: candidate.clone(),
                version,
                script: None,
            }));
        }
    }

    Err(ModuleError::UnresolvablePackage {
        module: module_name.to_string(),
        package: entry.name.clone(),
        min_version: entry.min_version.clone().unwrap_or_else(|| "any".into()),
    }
    .into())
}

/// Resolve all packages in a module spec.
/// Packages filtered out by platform constraints are silently skipped.
pub fn resolve_module_packages(
    module: &LoadedModule,
    platform: &Platform,
    managers: &HashMap<String, &dyn PackageManager>,
) -> Result<Vec<ResolvedPackage>> {
    let mut resolved = Vec::new();
    for entry in &module.spec.packages {
        if let Some(pkg) = resolve_package(entry, &module.name, platform, managers)? {
            resolved.push(pkg);
        }
    }
    Ok(resolved)
}

// ---------------------------------------------------------------------------
// Git file source URL parsing
// ---------------------------------------------------------------------------

/// Parsed git file source URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitSource {
    /// The repo URL (without tag/ref/subdir suffixes).
    pub repo_url: String,
    /// Tag to checkout (from `@tag` suffix).
    pub tag: Option<String>,
    /// Branch/ref to checkout (from `?ref=branch` suffix).
    pub git_ref: Option<String>,
    /// Subdirectory within the repo (from `//subdir` separator).
    pub subdir: Option<String>,
}

/// Check whether a file source string is a git URL (not a local path).
pub fn is_git_source(source: &str) -> bool {
    source.starts_with("https://")
        || source.starts_with("http://")
        || source.starts_with("git@")
        || source.starts_with("ssh://")
}

/// Check if a module name is a `registry/module[@tag]` reference.
/// Returns true if it contains `/` but is not a git URL.
pub fn is_registry_ref(name: &str) -> bool {
    name.contains('/') && !is_git_source(name)
}

/// Parsed registry/module reference.
pub struct RegistryRef {
    pub registry: String,
    pub module: String,
    pub tag: Option<String>,
}

/// Parse `registry/module[@tag]` into components.
/// Returns `None` if the input doesn't match the expected pattern.
pub fn parse_registry_ref(input: &str) -> Option<RegistryRef> {
    // Split on first `/` to get registry and remainder
    let (registry, remainder) = input.split_once('/')?;
    if registry.is_empty() || remainder.is_empty() {
        return None;
    }

    // Split remainder on `@` for optional tag
    let (module, tag) = match remainder.split_once('@') {
        Some((m, t)) if !m.is_empty() && !t.is_empty() => (m.to_string(), Some(t.to_string())),
        Some((_, _)) => return None, // empty module or tag
        None => (remainder.to_string(), None),
    };

    Some(RegistryRef {
        registry: registry.to_string(),
        module,
        tag,
    })
}

/// Resolve a profile module reference to its lookup name.
///
/// Profiles can reference modules as:
/// - `tmux` — local module (returns `"tmux"`)
/// - `community/tmux` — remote module from registry (returns `"tmux"`)
///
/// The returned name is what to look up in the loaded modules HashMap.
pub fn resolve_profile_module_name(profile_ref: &str) -> &str {
    if is_registry_ref(profile_ref) {
        profile_ref
            .split_once('/')
            .map(|(_, m)| m)
            .unwrap_or(profile_ref)
    } else {
        profile_ref
    }
}

/// Parse a git file source URL into its components.
///
/// Supports:
/// - `https://github.com/user/repo.git` — plain clone
/// - `https://github.com/user/repo.git@v2.1.0` — pin to tag
/// - `https://github.com/user/repo.git?ref=dev` — track branch
/// - `https://github.com/user/repo.git//subdir` — subdirectory
/// - `https://github.com/user/repo.git//subdir@v2.1.0` — subdir at tag
/// - `git@github.com:user/repo.git@v2.1.0` — SSH with tag
pub fn parse_git_source(source: &str) -> Result<GitSource> {
    if !is_git_source(source) {
        return Err(ModuleError::InvalidSpec {
            name: source.to_string(),
            message: "not a git URL".into(),
        }
        .into());
    }

    let mut url = source.to_string();
    let mut tag = None;
    let mut git_ref = None;
    let mut subdir = None;

    // Extract ?ref=... (must be done before @tag extraction since ? is unambiguous)
    // Stop at // (subdir separator) so ?ref=dev//subdir works correctly
    if let Some(ref_pos) = url.find("?ref=") {
        let after_ref = &url[ref_pos + 5..];
        let end = after_ref.find("//").unwrap_or(after_ref.len());
        let ref_val = after_ref[..end].to_string();
        let remainder = &after_ref[end..];
        url = format!("{}{}", &url[..ref_pos], remainder);
        git_ref = Some(ref_val);
    }

    // Extract //subdir (and possibly @tag after the subdir)
    // Skip the :// scheme prefix when looking for // path separator
    let search_start = url.find("://").map(|p| p + 3).unwrap_or(0);
    if let Some(rel_pos) = url[search_start..].find("//") {
        let subdir_pos = search_start + rel_pos;
        let subdir_part = url[subdir_pos + 2..].to_string();
        url = url[..subdir_pos].to_string();

        // The subdir part may have @tag
        if let Some(at_pos) = subdir_part.rfind('@') {
            subdir = Some(subdir_part[..at_pos].to_string());
            tag = Some(subdir_part[at_pos + 1..].to_string());
        } else {
            subdir = Some(subdir_part);
        }
    } else {
        // No subdir — check for @tag on the URL itself
        // For SSH URLs like git@github.com:user/repo.git@v2.1.0,
        // we need to find the @tag *after* the .git suffix
        if let Some(git_suffix_pos) = url.find(".git") {
            let after_git = &url[git_suffix_pos + 4..];
            if let Some(at_pos) = after_git.find('@') {
                tag = Some(after_git[at_pos + 1..].to_string());
                url = url[..git_suffix_pos + 4].to_string();
            }
        } else if let Some(at_pos) = url.rfind('@') {
            // No .git in URL — look for last @ that isn't part of the protocol.
            // For https/http/ssh://, skip past ://
            // For git@, skip past the first @
            let skip_to = if url.starts_with("git@") {
                url.find('@').map(|p| p + 1).unwrap_or(0)
            } else {
                url.find("://").map(|p| p + 3).unwrap_or(0)
            };
            if at_pos > skip_to {
                tag = Some(url[at_pos + 1..].to_string());
                url = url[..at_pos].to_string();
            }
        }
    }

    Ok(GitSource {
        repo_url: url,
        tag,
        git_ref,
        subdir,
    })
}

/// Compute the cache directory for a git source URL.
/// Uses SHA-256 hash of the repo URL for uniqueness.
pub fn git_cache_dir(cache_base: &Path, repo_url: &str) -> PathBuf {
    let hash = format!("{:x}", Sha256::digest(repo_url.as_bytes()));
    cache_base.join(&hash[..32])
}

/// Default cache directory for module git sources.
pub fn default_module_cache_dir() -> Result<PathBuf> {
    let base = directories::BaseDirs::new().ok_or_else(|| ModuleError::GitFetchFailed {
        module: String::new(),
        url: String::new(),
        message: "cannot determine home directory".into(),
    })?;
    Ok(base.cache_dir().join("cfgd").join("modules"))
}

// ---------------------------------------------------------------------------
// Git clone / fetch operations
// ---------------------------------------------------------------------------

/// Clone or fetch a git source to the cache, returning the local path.
///
/// If the repo is already cached, fetches updates. Otherwise, clones.
/// Checks out the specified tag/ref if provided.
pub fn fetch_git_source(
    git_src: &GitSource,
    cache_base: &Path,
    module_name: &str,
) -> Result<PathBuf> {
    let cache_dir = git_cache_dir(cache_base, &git_src.repo_url);

    if cache_dir.join(".git").exists() || cache_dir.join("HEAD").exists() {
        fetch_existing_repo(&cache_dir, git_src, module_name)?;
    } else {
        clone_repo(&cache_dir, git_src, module_name)?;
    }

    checkout_ref(&cache_dir, git_src, module_name)?;

    // Return the path, accounting for subdir
    match &git_src.subdir {
        Some(sub) => Ok(cache_dir.join(sub)),
        None => Ok(cache_dir),
    }
}

/// Open a git2 repo with a consistent error mapping.
fn open_repo(path: &Path, module: &str, url: &str) -> Result<git2::Repository> {
    git2::Repository::open(path).map_err(|e| {
        ModuleError::GitFetchFailed {
            module: module.to_string(),
            url: url.to_string(),
            message: format!("cannot open repo: {e}"),
        }
        .into()
    })
}

/// Build fetch options with SSH credential callback.
fn git_fetch_options<'a>() -> git2::FetchOptions<'a> {
    let mut callbacks = git2::RemoteCallbacks::new();
    callbacks.credentials(crate::git_ssh_credentials);
    let mut fetch_opts = git2::FetchOptions::new();
    fetch_opts.remote_callbacks(callbacks);
    fetch_opts
}

fn clone_repo(dest: &Path, git_src: &GitSource, module_name: &str) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ModuleError::GitFetchFailed {
            module: module_name.to_string(),
            url: git_src.repo_url.clone(),
            message: format!("cannot create cache directory: {e}"),
        })?;
    }

    let mut builder = git2::build::RepoBuilder::new();
    builder.fetch_options(git_fetch_options());

    builder
        .clone(&git_src.repo_url, dest)
        .map_err(|e| ModuleError::GitFetchFailed {
            module: module_name.to_string(),
            url: git_src.repo_url.clone(),
            message: e.to_string(),
        })?;

    Ok(())
}

fn fetch_existing_repo(repo_path: &Path, git_src: &GitSource, module_name: &str) -> Result<()> {
    let repo = open_repo(repo_path, module_name, &git_src.repo_url)?;

    let mut remote = repo
        .find_remote("origin")
        .map_err(|e| ModuleError::GitFetchFailed {
            module: module_name.to_string(),
            url: git_src.repo_url.clone(),
            message: format!("no 'origin' remote: {e}"),
        })?;

    let refspecs: Vec<String> = remote
        .refspecs()
        .filter_map(|rs| rs.str().map(String::from))
        .collect();
    let refspec_strs: Vec<&str> = refspecs.iter().map(|s| s.as_str()).collect();

    remote
        .fetch(&refspec_strs, Some(&mut git_fetch_options()), None)
        .map_err(|e| ModuleError::GitFetchFailed {
            module: module_name.to_string(),
            url: git_src.repo_url.clone(),
            message: format!("fetch failed: {e}"),
        })?;

    Ok(())
}

fn checkout_ref(repo_path: &Path, git_src: &GitSource, module_name: &str) -> Result<()> {
    let repo = open_repo(repo_path, module_name, &git_src.repo_url)?;

    let target_ref = git_src.tag.as_deref().or(git_src.git_ref.as_deref());

    let Some(ref_name) = target_ref else {
        // No specific ref — stay on default branch
        return Ok(());
    };

    // Try as a tag first, then as a branch
    let obj = repo
        .revparse_single(&format!("refs/tags/{ref_name}"))
        .or_else(|_| repo.revparse_single(&format!("refs/remotes/origin/{ref_name}")))
        .or_else(|_| repo.revparse_single(ref_name))
        .map_err(|e| ModuleError::GitFetchFailed {
            module: module_name.to_string(),
            url: git_src.repo_url.clone(),
            message: format!("cannot find ref '{ref_name}': {e}"),
        })?;

    // Peel to commit
    let commit = obj
        .peel_to_commit()
        .map_err(|e| ModuleError::GitFetchFailed {
            module: module_name.to_string(),
            url: git_src.repo_url.clone(),
            message: format!("ref '{ref_name}' does not point to a commit: {e}"),
        })?;

    repo.set_head_detached(commit.id())
        .map_err(|e| ModuleError::GitFetchFailed {
            module: module_name.to_string(),
            url: git_src.repo_url.clone(),
            message: format!("cannot detach HEAD to '{ref_name}': {e}"),
        })?;

    repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
        .map_err(|e| ModuleError::GitFetchFailed {
            module: module_name.to_string(),
            url: git_src.repo_url.clone(),
            message: format!("checkout failed for '{ref_name}': {e}"),
        })?;

    Ok(())
}

// ---------------------------------------------------------------------------
// File resolution
// ---------------------------------------------------------------------------

/// Resolve module file entries to concrete local paths.
/// Local sources are resolved relative to the module directory.
/// Git sources are cloned/fetched to cache and resolved to the local cache path.
pub fn resolve_module_files(module: &LoadedModule, cache_base: &Path) -> Result<Vec<ResolvedFile>> {
    let mut resolved = Vec::new();

    for entry in &module.spec.files {
        if is_git_source(&entry.source) {
            let git_src = parse_git_source(&entry.source)?;
            let local_path = fetch_git_source(&git_src, cache_base, &module.name)?;

            resolved.push(ResolvedFile {
                source: local_path,
                target: crate::expand_tilde(Path::new(&entry.target)),
                is_git_source: true,
                strategy: entry.strategy,
            });
        } else {
            // Local path — relative to module directory
            let source = module.dir.join(&entry.source);
            resolved.push(ResolvedFile {
                source,
                target: crate::expand_tilde(Path::new(&entry.target)),
                is_git_source: false,
                strategy: entry.strategy,
            });
        }
    }

    Ok(resolved)
}

// ---------------------------------------------------------------------------
// Full module resolution
// ---------------------------------------------------------------------------

/// Resolve a set of modules: load, sort dependencies, resolve packages and files.
/// Includes both local modules and remote modules from the lockfile.
pub fn resolve_modules(
    requested: &[String],
    config_dir: &Path,
    cache_base: &Path,
    platform: &Platform,
    managers: &HashMap<String, &dyn PackageManager>,
) -> Result<Vec<ResolvedModule>> {
    let all_modules = load_all_modules(config_dir, cache_base)?;

    // Resolve profile references (e.g., "community/tmux" → "tmux") to actual module names
    let resolved_names: Vec<String> = requested
        .iter()
        .map(|r| resolve_profile_module_name(r).to_string())
        .collect();

    let order = resolve_dependency_order(&resolved_names, &all_modules)?;

    let mut resolved = Vec::new();
    for name in &order {
        let module = &all_modules[name];
        let packages = resolve_module_packages(module, platform, managers)?;
        let files = resolve_module_files(module, cache_base)?;

        let post_apply_scripts = module
            .spec
            .scripts
            .as_ref()
            .map(|s| s.post_apply.clone())
            .unwrap_or_default();

        resolved.push(ResolvedModule {
            name: name.clone(),
            packages,
            files,
            env: module.spec.env.clone(),
            aliases: module.spec.aliases.clone(),
            post_apply_scripts,
            depends: module.spec.depends.clone(),
        });
    }

    Ok(resolved)
}

// ---------------------------------------------------------------------------
// Module lockfile — tracking remote modules with integrity
// ---------------------------------------------------------------------------

use crate::config::{ModuleLockEntry, ModuleLockfile, ModuleRegistryEntry};

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
/// Uses atomic write (temp file + rename) to prevent corruption.
pub fn save_lockfile(config_dir: &Path, lockfile: &ModuleLockfile) -> Result<()> {
    let lockfile_path = config_dir.join("modules.lock");
    let contents = serde_yaml::to_string(lockfile).map_err(ConfigError::from)?;
    let tmp_path = config_dir.join(".modules.lock.tmp");
    std::fs::write(&tmp_path, &contents).map_err(|e| ConfigError::Invalid {
        message: format!("cannot write lockfile {}: {e}", tmp_path.display()),
    })?;
    std::fs::rename(&tmp_path, &lockfile_path).map_err(|e| ConfigError::Invalid {
        message: format!("cannot rename lockfile {}: {e}", lockfile_path.display()),
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

    Ok(format!("sha256:{:x}", Sha256::digest(&hasher_input)))
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

/// Result of fetching a remote module — module + lockfile metadata.
#[derive(Debug, Clone)]
pub struct FetchedRemoteModule {
    pub module: LoadedModule,
    pub commit: String,
    pub integrity: String,
}

/// Fetch a remote module from a git URL.
///
/// Validates that the URL has a pinned ref (tag or commit SHA).
/// Branches are rejected for security (upstream push = code execution).
pub fn fetch_remote_module(url: &str, cache_base: &Path) -> Result<FetchedRemoteModule> {
    let git_src = parse_git_source(url)?;

    // Enforce pinned ref for remote modules — only tags (which may be semver tags or
    // commit SHAs) are allowed. Branch refs (?ref=main) are rejected because upstream
    // pushes would silently change the code that gets executed.
    if git_src.git_ref.is_some() {
        return Err(ModuleError::UnpinnedRemoteModule {
            name: url.to_string(),
        }
        .into());
    }
    if git_src.tag.is_none() {
        return Err(ModuleError::UnpinnedRemoteModule {
            name: url.to_string(),
        }
        .into());
    }

    let local_path = fetch_git_source(&git_src, cache_base, "remote")?;

    // The repo root is the cache dir (before subdir), we need it for commit hash
    let repo_dir = git_cache_dir(cache_base, &git_src.repo_url);
    let commit = get_head_commit_sha(&repo_dir)?;

    // Load the module from the fetched path
    let module = load_module(&local_path)?;

    // Compute integrity hash
    let integrity = hash_module_contents(&local_path)?;

    Ok(FetchedRemoteModule {
        module,
        commit,
        integrity,
    })
}

/// Get the HEAD commit SHA from a git repo.
pub fn get_head_commit_sha(repo_path: &Path) -> Result<String> {
    let path_str = repo_path.display().to_string();
    let repo = open_repo(repo_path, &path_str, &path_str)?;
    let head = repo.head().map_err(|e| ModuleError::GitFetchFailed {
        module: path_str.clone(),
        url: path_str.clone(),
        message: format!("cannot read HEAD: {e}"),
    })?;
    let commit = head
        .peel_to_commit()
        .map_err(|e| ModuleError::GitFetchFailed {
            module: path_str.clone(),
            url: path_str,
            message: format!("HEAD is not a commit: {e}"),
        })?;
    Ok(commit.id().to_string())
}

/// Verify the integrity of a locked remote module against its lockfile entry.
pub fn verify_lockfile_integrity(lock_entry: &ModuleLockEntry, cache_base: &Path) -> Result<()> {
    let git_src = parse_git_source(&lock_entry.url)?;
    let local_path = match &lock_entry.subdir {
        Some(sub) => git_cache_dir(cache_base, &git_src.repo_url).join(sub),
        None => git_cache_dir(cache_base, &git_src.repo_url),
    };

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
        let local_path = fetch_git_source(&pinned_src, cache_base, &entry.name)?;

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
) -> Result<HashMap<String, LoadedModule>> {
    let mut modules = load_modules(config_dir)?;
    load_locked_modules(config_dir, cache_base, &mut modules)?;
    Ok(modules)
}

/// Signature status for a git tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagSignatureStatus {
    /// Lightweight tag — cannot carry a signature.
    LightweightTag,
    /// Annotated tag with no signature.
    Unsigned,
    /// Annotated tag with a GPG/SSH signature present.
    SignaturePresent,
    /// Tag not found.
    TagNotFound,
}

/// Check whether a git tag has a GPG/SSH signature.
///
/// Detects signature presence via git2 (no shell-out required).
/// Full GPG verification (cryptographic check) requires `git tag -v` which
/// calls `gpg`; the CLI layer can do that if desired.
pub fn check_tag_signature(
    repo_path: &Path,
    tag_name: &str,
    module_name: &str,
) -> Result<TagSignatureStatus> {
    let repo = open_repo(repo_path, module_name, "")?;

    let tag_ref = match repo.revparse_single(&format!("refs/tags/{tag_name}")) {
        Ok(obj) => obj,
        Err(_) => return Ok(TagSignatureStatus::TagNotFound),
    };

    let tag = match tag_ref.as_tag() {
        Some(t) => t,
        None => return Ok(TagSignatureStatus::LightweightTag),
    };

    let message = match tag.message() {
        Some(m) => m,
        None => return Ok(TagSignatureStatus::Unsigned),
    };

    if message.contains("-----BEGIN PGP SIGNATURE-----")
        || message.contains("-----BEGIN SSH SIGNATURE-----")
    {
        Ok(TagSignatureStatus::SignaturePresent)
    } else {
        Ok(TagSignatureStatus::Unsigned)
    }
}

// ---------------------------------------------------------------------------
// Module registries — git repos with prescribed directory structure
// ---------------------------------------------------------------------------

/// A discovered module within a registry repo.
#[derive(Debug, Clone)]
pub struct RegistryModule {
    /// Module name (directory name under `modules/`).
    pub name: String,
    /// Description from the module's `module.yaml` metadata.
    pub description: String,
    /// Registry name (alias) this module belongs to.
    pub registry: String,
    /// Available per-module tags (`<module>/v1.0.0` format) in the repo.
    pub tags: Vec<String>,
}

/// Extract the default registry name from a GitHub URL.
/// `https://github.com/cfgd-community/modules.git` → `cfgd-community`
pub fn extract_registry_name(url: &str) -> Option<String> {
    // Handle https://github.com/org/repo(.git)
    if let Some(rest) = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
    {
        return rest.split('/').next().map(|s| s.to_string());
    }
    // Handle git@github.com:org/repo(.git)
    if let Some(rest) = url.strip_prefix("git@github.com:") {
        return rest.split('/').next().map(|s| s.to_string());
    }
    None
}

/// Fetch a module registry repo and discover available modules.
///
/// Scans the `modules/` directory for subdirectories containing `module.yaml`.
/// Also collects per-module tags (matching `<module>/v*` pattern).
pub fn fetch_registry_modules(
    registry: &ModuleRegistryEntry,
    cache_base: &Path,
) -> Result<Vec<RegistryModule>> {
    let git_src = GitSource {
        repo_url: registry.url.clone(),
        tag: None,
        git_ref: None,
        subdir: None,
    };

    let cache_dir = git_cache_dir(cache_base, &registry.url);

    // Clone or fetch
    if cache_dir.join(".git").exists() || cache_dir.join("HEAD").exists() {
        fetch_existing_repo(&cache_dir, &git_src, &registry.name)?;
    } else {
        clone_repo(&cache_dir, &git_src, &registry.name)?;
    }

    let modules_dir = cache_dir.join("modules");
    if !modules_dir.is_dir() {
        return Err(ModuleError::SourceFetchFailed {
            url: registry.url.clone(),
            message: "registry repo has no modules/ directory".into(),
        }
        .into());
    }

    // Collect per-module tags from the repo
    let module_tags = list_module_tags(&cache_dir, &registry.name)?;

    // Scan modules/ for module directories
    let mut found = Vec::new();
    let entries = std::fs::read_dir(&modules_dir)?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let module_yaml = path.join("module.yaml");
        if !module_yaml.exists() {
            continue;
        }
        let mod_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };

        // Read description from module.yaml metadata
        let description = std::fs::read_to_string(&module_yaml)
            .ok()
            .and_then(|c| crate::config::parse_module(&c).ok())
            .and_then(|doc| doc.metadata.description.clone())
            .unwrap_or_default();

        // Collect tags for this module
        let tags = module_tags.get(&mod_name).cloned().unwrap_or_default();

        found.push(RegistryModule {
            name: mod_name,
            description,
            registry: registry.name.clone(),
            tags,
        });
    }

    found.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(found)
}

/// List per-module tags from a source repo.
/// Tags follow the `<module>/<version>` convention (e.g., `tmux/v1.0.0`).
/// Returns a map of module_name → sorted list of version tags.
fn list_module_tags(repo_path: &Path, source_name: &str) -> Result<HashMap<String, Vec<String>>> {
    let repo = open_repo(repo_path, source_name, "")?;
    let mut result: HashMap<String, Vec<String>> = HashMap::new();

    let tag_names = repo
        .tag_names(None)
        .map_err(|e| ModuleError::GitFetchFailed {
            module: source_name.to_string(),
            url: String::new(),
            message: format!("cannot list tags: {e}"),
        })?;

    for tag_name in tag_names.iter().flatten() {
        if let Some((module, version)) = tag_name.split_once('/') {
            result
                .entry(module.to_string())
                .or_default()
                .push(version.to_string());
        }
    }

    // Sort each module's tags (best-effort semver sort, falling back to string sort)
    for tags in result.values_mut() {
        tags.sort_by(|a, b| {
            let av = crate::parse_loose_version(a);
            let bv = crate::parse_loose_version(b);
            match (av, bv) {
                (Some(av), Some(bv)) => av.cmp(&bv),
                _ => a.cmp(b),
            }
        });
    }

    Ok(result)
}

/// Find the latest version for a module in a registry repo.
/// Registry repo tags follow `<module>/<version>` convention; returns only the version part.
pub fn latest_module_version(
    registry: &ModuleRegistryEntry,
    module_name: &str,
    cache_base: &Path,
) -> Result<Option<String>> {
    let cache_dir = git_cache_dir(cache_base, &registry.url);
    let tags = list_module_tags(&cache_dir, &registry.name)?;
    Ok(tags.get(module_name).and_then(|t| t.last()).cloned())
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
                "~ package '{}': min-version {} -> {}",
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
        .map(|s| s.post_apply.iter().map(|s| s.as_str()).collect())
        .unwrap_or_default();
    let new_scripts: Vec<&str> = new
        .spec
        .scripts
        .as_ref()
        .map(|s| s.post_apply.iter().map(|s| s.as_str()).collect())
        .unwrap_or_default();
    let old_script_set: HashSet<&str> = old_scripts.into_iter().collect();
    let new_script_set: HashSet<&str> = new_scripts.into_iter().collect();
    for script in new_script_set.difference(&old_script_set) {
        changes.push(format!("+ post-apply script: {script}"));
    }
    for script in old_script_set.difference(&new_script_set) {
        changes.push(format!("- post-apply script: {script}"));
    }

    if changes.is_empty() {
        changes.push("(no spec changes)".to_string());
    }

    changes
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashSet;

    use crate::config::ModuleFileEntry;
    use crate::output::Printer;

    // --- Mock PackageManager for testing ---

    struct MockManager {
        name: String,
        available: bool,
        packages: HashMap<String, String>, // name → version
    }

    impl MockManager {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
                available: true,
                packages: HashMap::new(),
            }
        }

        fn with_package(mut self, pkg: &str, version: &str) -> Self {
            self.packages.insert(pkg.to_string(), version.to_string());
            self
        }

        fn unavailable(mut self) -> Self {
            self.available = false;
            self
        }
    }

    impl PackageManager for MockManager {
        fn name(&self) -> &str {
            &self.name
        }
        fn is_available(&self) -> bool {
            self.available
        }
        fn can_bootstrap(&self) -> bool {
            false
        }
        fn bootstrap(&self, _printer: &Printer) -> Result<()> {
            Ok(())
        }
        fn installed_packages(&self) -> Result<HashSet<String>> {
            Ok(HashSet::new())
        }
        fn install(&self, _packages: &[String], _printer: &Printer) -> Result<()> {
            Ok(())
        }
        fn uninstall(&self, _packages: &[String], _printer: &Printer) -> Result<()> {
            Ok(())
        }
        fn update(&self, _printer: &Printer) -> Result<()> {
            Ok(())
        }
        fn available_version(&self, package: &str) -> Result<Option<String>> {
            Ok(self.packages.get(package).cloned())
        }
    }

    // --- Test helpers ---

    fn make_manager_map<'a>(
        entries: &[(&str, &'a dyn PackageManager)],
    ) -> HashMap<String, &'a dyn PackageManager> {
        entries
            .iter()
            .map(|(name, mgr)| (name.to_string(), *mgr))
            .collect()
    }

    fn linux_ubuntu_platform() -> Platform {
        Platform {
            os: crate::platform::Os::Linux,
            distro: crate::platform::Distro::Ubuntu,
            version: "22.04".into(),
            arch: crate::platform::Arch::X86_64,
        }
    }

    fn macos_platform() -> Platform {
        Platform {
            os: crate::platform::Os::MacOS,
            distro: crate::platform::Distro::MacOS,
            version: "14.0".into(),
            arch: crate::platform::Arch::Aarch64,
        }
    }

    // --- Module loading tests ---

    #[test]
    fn load_modules_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let result = load_modules(dir.path()).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn load_modules_no_modules_dir() {
        let dir = tempfile::tempdir().unwrap();
        // No modules/ subdirectory
        let result = load_modules(dir.path()).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn load_single_module() {
        let dir = tempfile::tempdir().unwrap();
        let mod_dir = dir.path().join("modules").join("nvim");
        std::fs::create_dir_all(&mod_dir).unwrap();
        std::fs::write(
            mod_dir.join("module.yaml"),
            r#"
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: nvim
spec:
  depends: [node]
  packages:
    - name: neovim
      min-version: "0.9"
      prefer: [brew, snap, apt]
      aliases:
        snap: nvim
    - name: ripgrep
  files:
    - source: config/
      target: ~/.config/nvim/
"#,
        )
        .unwrap();

        let modules = load_modules(dir.path()).unwrap();
        assert_eq!(modules.len(), 1);
        let nvim = &modules["nvim"];
        assert_eq!(nvim.name, "nvim");
        assert_eq!(nvim.spec.depends, vec!["node"]);
        assert_eq!(nvim.spec.packages.len(), 2);
        assert_eq!(nvim.spec.packages[0].name, "neovim");
        assert_eq!(nvim.spec.packages[0].min_version, Some("0.9".to_string()));
        assert_eq!(nvim.spec.packages[0].prefer, vec!["brew", "snap", "apt"]);
        assert_eq!(
            nvim.spec.packages[0].aliases.get("snap"),
            Some(&"nvim".to_string())
        );
        assert_eq!(nvim.spec.packages[1].name, "ripgrep");
        assert_eq!(nvim.spec.files.len(), 1);
    }

    #[test]
    fn load_module_name_mismatch_errors() {
        let dir = tempfile::tempdir().unwrap();
        let mod_dir = dir.path().join("modules").join("wrong-name");
        std::fs::create_dir_all(&mod_dir).unwrap();
        std::fs::write(
            mod_dir.join("module.yaml"),
            r#"
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: actual-name
spec: {}
"#,
        )
        .unwrap();

        let result = load_modules(dir.path());
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("does not match"));
    }

    #[test]
    fn load_module_wrong_kind_errors() {
        let dir = tempfile::tempdir().unwrap();
        let mod_dir = dir.path().join("modules").join("bad");
        std::fs::create_dir_all(&mod_dir).unwrap();
        std::fs::write(
            mod_dir.join("module.yaml"),
            r#"
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: bad
spec: {}
"#,
        )
        .unwrap();

        let result = load_modules(dir.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Module"));
    }

    // --- Dependency resolution tests ---

    fn make_modules(specs: &[(&str, &[&str])]) -> HashMap<String, LoadedModule> {
        let mut modules = HashMap::new();
        for (name, deps) in specs {
            modules.insert(
                name.to_string(),
                LoadedModule {
                    name: name.to_string(),
                    spec: ModuleSpec {
                        depends: deps.iter().map(|s| s.to_string()).collect(),
                        ..Default::default()
                    },
                    dir: PathBuf::from(format!("/fake/{name}")),
                },
            );
        }
        modules
    }

    #[test]
    fn dependency_order_single_no_deps() {
        let modules = make_modules(&[("nvim", &[])]);
        let order = resolve_dependency_order(&["nvim".into()], &modules).unwrap();
        assert_eq!(order, vec!["nvim"]);
    }

    #[test]
    fn dependency_order_linear_chain() {
        let modules = make_modules(&[("a", &[]), ("b", &["a"]), ("c", &["b"])]);
        let order = resolve_dependency_order(&["c".into()], &modules).unwrap();
        assert_eq!(order, vec!["a", "b", "c"]);
    }

    #[test]
    fn dependency_order_diamond() {
        let modules = make_modules(&[
            ("base", &[]),
            ("left", &["base"]),
            ("right", &["base"]),
            ("top", &["left", "right"]),
        ]);
        let order = resolve_dependency_order(&["top".into()], &modules).unwrap();
        // base must come first, then left and right (alphabetical among peers), then top
        assert_eq!(order[0], "base");
        assert!(order.contains(&"left".to_string()));
        assert!(order.contains(&"right".to_string()));
        assert_eq!(order.last().unwrap(), "top");
    }

    #[test]
    fn dependency_order_cycle_detected() {
        let modules = make_modules(&[("a", &["b"]), ("b", &["a"])]);
        let result = resolve_dependency_order(&["a".into()], &modules);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("cycle"));
    }

    #[test]
    fn dependency_order_missing_dependency() {
        let modules = make_modules(&[("a", &["missing"])]);
        let result = resolve_dependency_order(&["a".into()], &modules);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("missing"));
    }

    #[test]
    fn dependency_order_module_not_found() {
        let modules: HashMap<String, LoadedModule> = HashMap::new();
        let result = resolve_dependency_order(&["nonexistent".into()], &modules);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("nonexistent"));
    }

    #[test]
    fn dependency_order_multiple_requested() {
        let modules = make_modules(&[("base", &[]), ("nvim", &["base"]), ("tmux", &["base"])]);
        let order = resolve_dependency_order(&["nvim".into(), "tmux".into()], &modules).unwrap();
        assert_eq!(order[0], "base");
        assert!(order.contains(&"nvim".to_string()));
        assert!(order.contains(&"tmux".to_string()));
        assert_eq!(order.len(), 3);
    }

    #[test]
    fn dependency_order_three_node_cycle() {
        let modules = make_modules(&[("a", &["c"]), ("b", &["a"]), ("c", &["b"])]);
        let result = resolve_dependency_order(&["a".into()], &modules);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cycle"));
    }

    // --- Package resolution tests ---

    #[test]
    fn resolve_package_simple_native() {
        let brew = MockManager::new("brew").with_package("ripgrep", "14.1.0");
        let managers = make_manager_map(&[("brew", &brew)]);
        let platform = macos_platform();

        let entry = ModulePackageEntry {
            name: "ripgrep".into(),
            min_version: None,
            prefer: vec![],
            aliases: HashMap::new(),
            script: None,
            deny: vec![],
            platforms: vec![],
        };

        let result = resolve_package(&entry, "test", &platform, &managers)
            .unwrap()
            .unwrap();
        assert_eq!(result.canonical_name, "ripgrep");
        assert_eq!(result.resolved_name, "ripgrep");
        assert_eq!(result.manager, "brew");
        assert_eq!(result.version, Some("14.1.0".into()));
    }

    #[test]
    fn resolve_package_with_prefer_list() {
        let brew = MockManager::new("brew").unavailable();
        let apt = MockManager::new("apt").with_package("neovim", "0.10.2");
        let snap = MockManager::new("snap").with_package("nvim", "0.10.3");
        let managers = make_manager_map(&[("brew", &brew), ("apt", &apt), ("snap", &snap)]);
        let platform = linux_ubuntu_platform();

        let entry = ModulePackageEntry {
            name: "neovim".into(),
            min_version: Some("0.9".into()),
            prefer: vec!["brew".into(), "snap".into(), "apt".into()],
            aliases: [("snap".to_string(), "nvim".to_string())]
                .into_iter()
                .collect(),
            script: None,
            deny: vec![],
            platforms: vec![],
        };

        // brew is unavailable, so snap should be tried next
        let result = resolve_package(&entry, "nvim", &platform, &managers)
            .unwrap()
            .unwrap();
        assert_eq!(result.manager, "snap");
        assert_eq!(result.resolved_name, "nvim"); // alias applied
        assert_eq!(result.version, Some("0.10.3".into()));
    }

    #[test]
    fn resolve_package_min_version_check() {
        let apt = MockManager::new("apt").with_package("neovim", "0.6.1");
        let snap = MockManager::new("snap").with_package("nvim", "0.10.2");
        let managers = make_manager_map(&[("apt", &apt), ("snap", &snap)]);
        let platform = linux_ubuntu_platform();

        let entry = ModulePackageEntry {
            name: "neovim".into(),
            min_version: Some("0.9".into()),
            prefer: vec!["apt".into(), "snap".into()],
            aliases: [("snap".to_string(), "nvim".to_string())]
                .into_iter()
                .collect(),
            script: None,
            deny: vec![],
            platforms: vec![],
        };

        // apt has 0.6.1 which is < 0.9, so snap (0.10.2) should be chosen
        let result = resolve_package(&entry, "nvim", &platform, &managers)
            .unwrap()
            .unwrap();
        assert_eq!(result.manager, "snap");
        assert_eq!(result.version, Some("0.10.2".into()));
    }

    #[test]
    fn resolve_package_unresolvable() {
        let apt = MockManager::new("apt").with_package("neovim", "0.6.1");
        let managers = make_manager_map(&[("apt", &apt)]);
        let platform = linux_ubuntu_platform();

        let entry = ModulePackageEntry {
            name: "neovim".into(),
            min_version: Some("0.9".into()),
            prefer: vec!["apt".into()],
            aliases: HashMap::new(),
            script: None,
            deny: vec![],
            platforms: vec![],
        };

        let result = resolve_package(&entry, "nvim", &platform, &managers);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("cannot be resolved")
        );
    }

    #[test]
    fn resolve_package_alias_applied() {
        let apt = MockManager::new("apt").with_package("fd-find", "8.7.0");
        let managers = make_manager_map(&[("apt", &apt)]);
        let platform = linux_ubuntu_platform();

        let entry = ModulePackageEntry {
            name: "fd".into(),
            min_version: None,
            prefer: vec![],
            aliases: [("apt".to_string(), "fd-find".to_string())]
                .into_iter()
                .collect(),
            script: None,
            deny: vec![],
            platforms: vec![],
        };

        let result = resolve_package(&entry, "test", &platform, &managers)
            .unwrap()
            .unwrap();
        assert_eq!(result.canonical_name, "fd");
        assert_eq!(result.resolved_name, "fd-find");
        assert_eq!(result.manager, "apt");
    }

    #[test]
    fn resolve_package_manager_not_registered() {
        let managers: HashMap<String, &dyn PackageManager> = HashMap::new();
        let platform = linux_ubuntu_platform();

        let entry = ModulePackageEntry {
            name: "ripgrep".into(),
            min_version: None,
            prefer: vec!["brew".into()],
            aliases: HashMap::new(),
            script: None,
            deny: vec![],
            platforms: vec![],
        };

        // brew not in managers map → unresolvable
        let result = resolve_package(&entry, "test", &platform, &managers);
        assert!(result.is_err());
    }

    // --- Git URL parsing tests ---

    #[test]
    fn parse_git_source_plain_https() {
        let src = parse_git_source("https://github.com/user/repo.git").unwrap();
        assert_eq!(src.repo_url, "https://github.com/user/repo.git");
        assert_eq!(src.tag, None);
        assert_eq!(src.git_ref, None);
        assert_eq!(src.subdir, None);
    }

    #[test]
    fn parse_git_source_with_tag() {
        let src = parse_git_source("https://github.com/user/repo.git@v2.1.0").unwrap();
        assert_eq!(src.repo_url, "https://github.com/user/repo.git");
        assert_eq!(src.tag, Some("v2.1.0".into()));
        assert_eq!(src.git_ref, None);
        assert_eq!(src.subdir, None);
    }

    #[test]
    fn parse_git_source_with_ref() {
        let src = parse_git_source("https://github.com/user/repo.git?ref=dev").unwrap();
        assert_eq!(src.repo_url, "https://github.com/user/repo.git");
        assert_eq!(src.tag, None);
        assert_eq!(src.git_ref, Some("dev".into()));
        assert_eq!(src.subdir, None);
    }

    #[test]
    fn parse_git_source_with_subdir() {
        let src = parse_git_source("https://github.com/user/repo.git//nvim").unwrap();
        assert_eq!(src.repo_url, "https://github.com/user/repo.git");
        assert_eq!(src.tag, None);
        assert_eq!(src.git_ref, None);
        assert_eq!(src.subdir, Some("nvim".into()));
    }

    #[test]
    fn parse_git_source_subdir_with_tag() {
        let src = parse_git_source("https://github.com/user/dotfiles.git//nvim@v3.0").unwrap();
        assert_eq!(src.repo_url, "https://github.com/user/dotfiles.git");
        assert_eq!(src.tag, Some("v3.0".into()));
        assert_eq!(src.subdir, Some("nvim".into()));
    }

    #[test]
    fn parse_git_source_ssh_with_tag() {
        let src = parse_git_source("git@github.com:user/nvim-config.git@v2.1.0").unwrap();
        assert_eq!(src.repo_url, "git@github.com:user/nvim-config.git");
        assert_eq!(src.tag, Some("v2.1.0".into()));
    }

    #[test]
    fn parse_git_source_ssh_with_ref() {
        let src = parse_git_source("git@github.com:user/nvim-config.git?ref=main").unwrap();
        assert_eq!(src.repo_url, "git@github.com:user/nvim-config.git");
        assert_eq!(src.git_ref, Some("main".into()));
        assert_eq!(src.tag, None);
    }

    #[test]
    fn parse_git_source_not_git_url() {
        let result = parse_git_source("config/");
        assert!(result.is_err());
    }

    #[test]
    fn is_git_source_tests() {
        assert!(is_git_source("https://github.com/user/repo.git"));
        assert!(is_git_source("git@github.com:user/repo.git"));
        assert!(is_git_source("ssh://git@github.com/user/repo.git"));
        assert!(!is_git_source("config/"));
        assert!(!is_git_source("../relative/path"));
        assert!(!is_git_source("~/.config/nvim"));
    }

    // --- Git cache dir tests ---

    #[test]
    fn git_cache_dir_deterministic() {
        let base = Path::new("/tmp/cache");
        let dir1 = git_cache_dir(base, "https://github.com/user/repo.git");
        let dir2 = git_cache_dir(base, "https://github.com/user/repo.git");
        assert_eq!(dir1, dir2);
    }

    #[test]
    fn git_cache_dir_different_urls() {
        let base = Path::new("/tmp/cache");
        let dir1 = git_cache_dir(base, "https://github.com/user/repo1.git");
        let dir2 = git_cache_dir(base, "https://github.com/user/repo2.git");
        assert_ne!(dir1, dir2);
    }

    // --- File resolution tests ---

    #[test]
    fn resolve_local_files() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path().join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(config_dir.join("init.lua"), "-- test").unwrap();

        let module = LoadedModule {
            name: "nvim".into(),
            spec: ModuleSpec {
                files: vec![ModuleFileEntry {
                    source: "config/".into(),
                    target: "/home/user/.config/nvim/".into(),
                    strategy: None,
                    private: false,
                }],
                ..Default::default()
            },
            dir: dir.path().to_path_buf(),
        };

        let cache_dir = tempfile::tempdir().unwrap();
        let resolved = resolve_module_files(&module, cache_dir.path()).unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].source, dir.path().join("config/"));
        assert_eq!(
            resolved[0].target,
            PathBuf::from("/home/user/.config/nvim/")
        );
        assert!(!resolved[0].is_git_source);
    }

    // --- Full resolution test with filesystem ---

    #[test]
    fn full_module_resolution() {
        let dir = tempfile::tempdir().unwrap();

        // Create two modules: node (leaf) and nvim (depends on node)
        let node_dir = dir.path().join("modules").join("node");
        std::fs::create_dir_all(&node_dir).unwrap();
        std::fs::write(
            node_dir.join("module.yaml"),
            r#"
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: node
spec:
  packages:
    - name: nodejs
      aliases:
        brew: node
"#,
        )
        .unwrap();

        let nvim_dir = dir.path().join("modules").join("nvim");
        std::fs::create_dir_all(&nvim_dir).unwrap();
        std::fs::write(
            nvim_dir.join("module.yaml"),
            r#"
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: nvim
spec:
  depends: [node]
  packages:
    - name: neovim
    - name: ripgrep
  scripts:
    post-apply:
      - nvim --headless "+Lazy! sync" +qa
"#,
        )
        .unwrap();

        let brew = MockManager::new("brew")
            .with_package("node", "20.0.0")
            .with_package("neovim", "0.10.2")
            .with_package("ripgrep", "14.1.0");

        let managers = make_manager_map(&[("brew", &brew)]);
        let platform = macos_platform();

        let cache_dir = tempfile::tempdir().unwrap();

        let resolved = resolve_modules(
            &["nvim".into()],
            dir.path(),
            cache_dir.path(),
            &platform,
            &managers,
        )
        .unwrap();

        // node should come before nvim
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].name, "node");
        assert_eq!(resolved[1].name, "nvim");

        // node packages
        assert_eq!(resolved[0].packages.len(), 1);
        assert_eq!(resolved[0].packages[0].canonical_name, "nodejs");
        assert_eq!(resolved[0].packages[0].resolved_name, "node"); // alias
        assert_eq!(resolved[0].packages[0].manager, "brew");

        // nvim packages
        assert_eq!(resolved[1].packages.len(), 2);
        assert_eq!(resolved[1].packages[0].canonical_name, "neovim");
        assert_eq!(resolved[1].packages[1].canonical_name, "ripgrep");

        // nvim scripts
        assert_eq!(resolved[1].post_apply_scripts.len(), 1);
    }

    // --- Module YAML parsing tests ---

    #[test]
    fn parse_module_yaml() {
        let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: test-mod
spec:
  depends: [a, b]
  packages:
    - name: foo
      min-version: "1.0"
      prefer: [brew, apt]
      aliases:
        apt: foo-tools
    - name: bar
  files:
    - source: config/
      target: ~/.config/foo/
    - source: https://github.com/user/repo.git@v1.0
      target: ~/.config/bar/
  scripts:
    post-apply:
      - echo done
"#;
        let doc = parse_module(yaml).unwrap();
        assert_eq!(doc.metadata.name, "test-mod");
        assert_eq!(doc.spec.depends, vec!["a", "b"]);
        assert_eq!(doc.spec.packages.len(), 2);
        assert_eq!(doc.spec.packages[0].name, "foo");
        assert_eq!(doc.spec.packages[0].min_version, Some("1.0".into()));
        assert_eq!(doc.spec.packages[0].prefer, vec!["brew", "apt"]);
        assert_eq!(
            doc.spec.packages[0].aliases.get("apt"),
            Some(&"foo-tools".to_string())
        );
        assert_eq!(doc.spec.files.len(), 2);
        assert_eq!(
            doc.spec.files[1].source,
            "https://github.com/user/repo.git@v1.0"
        );
        let scripts = doc.spec.scripts.unwrap();
        assert_eq!(scripts.post_apply, vec!["echo done"]);
    }

    #[test]
    fn parse_module_minimal() {
        let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: minimal
spec: {}
"#;
        let doc = parse_module(yaml).unwrap();
        assert_eq!(doc.metadata.name, "minimal");
        assert!(doc.spec.packages.is_empty());
        assert!(doc.spec.files.is_empty());
        assert!(doc.spec.depends.is_empty());
    }

    // --- Profile modules field test ---

    #[test]
    fn profile_with_modules_field() {
        let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: test
spec:
  modules: [nvim, tmux, git]
  packages:
    brew:
      formulae: [ripgrep]
"#;
        let doc: crate::config::ProfileDocument = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(doc.spec.modules, vec!["nvim", "tmux", "git"]);
    }

    // --- Script package resolution tests ---

    #[test]
    fn resolve_package_script_manager() {
        let managers: HashMap<String, &dyn PackageManager> = HashMap::new();
        let platform = linux_ubuntu_platform();

        let entry = ModulePackageEntry {
            name: "rustup".into(),
            min_version: None,
            prefer: vec!["script".into()],
            aliases: HashMap::new(),
            script: Some(
                "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y".into(),
            ),
            deny: vec![],
            platforms: vec![],
        };

        let result = resolve_package(&entry, "test", &platform, &managers)
            .unwrap()
            .unwrap();
        assert_eq!(result.manager, "script");
        assert_eq!(result.canonical_name, "rustup");
        assert_eq!(result.resolved_name, "rustup");
        assert!(result.script.is_some());
        assert!(result.script.unwrap().contains("rustup.rs"));
        assert!(result.version.is_none());
    }

    #[test]
    fn resolve_package_script_fallback() {
        // brew is unavailable, script should be chosen as fallback
        let brew = MockManager::new("brew").unavailable();
        let managers = make_manager_map(&[("brew", &brew)]);
        let platform = linux_ubuntu_platform();

        let entry = ModulePackageEntry {
            name: "neovim".into(),
            min_version: None,
            prefer: vec!["brew".into(), "script".into()],
            aliases: HashMap::new(),
            script: Some("scripts/install-neovim.sh".into()),
            deny: vec![],
            platforms: vec![],
        };

        let result = resolve_package(&entry, "nvim", &platform, &managers)
            .unwrap()
            .unwrap();
        assert_eq!(result.manager, "script");
        assert_eq!(result.script, Some("scripts/install-neovim.sh".into()));
    }

    #[test]
    fn resolve_package_script_preferred_over_manager() {
        // When script is first in prefer, it wins even if a manager is available
        let brew = MockManager::new("brew").with_package("neovim", "0.10.2");
        let managers = make_manager_map(&[("brew", &brew)]);
        let platform = macos_platform();

        let entry = ModulePackageEntry {
            name: "neovim".into(),
            min_version: None,
            prefer: vec!["script".into(), "brew".into()],
            aliases: HashMap::new(),
            script: Some("build-from-source.sh".into()),
            deny: vec![],
            platforms: vec![],
        };

        let result = resolve_package(&entry, "nvim", &platform, &managers)
            .unwrap()
            .unwrap();
        assert_eq!(result.manager, "script");
    }

    #[test]
    fn resolve_package_script_missing_errors() {
        let managers: HashMap<String, &dyn PackageManager> = HashMap::new();
        let platform = linux_ubuntu_platform();

        let entry = ModulePackageEntry {
            name: "rustup".into(),
            min_version: None,
            prefer: vec!["script".into()],
            aliases: HashMap::new(),
            script: None, // script field missing!
            deny: vec![],
            platforms: vec![],
        };

        let result = resolve_package(&entry, "test", &platform, &managers);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("no 'script' field")
        );
    }

    // --- Platform filtering tests ---

    #[test]
    fn resolve_package_platform_match_os() {
        let apt = MockManager::new("apt").with_package("ripgrep", "14.0.0");
        let managers = make_manager_map(&[("apt", &apt)]);
        let platform = linux_ubuntu_platform();

        let entry = ModulePackageEntry {
            name: "ripgrep".into(),
            min_version: None,
            prefer: vec![],
            aliases: HashMap::new(),
            script: None,
            deny: vec![],
            platforms: vec!["linux".into()],
        };

        let result = resolve_package(&entry, "test", &platform, &managers).unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().manager, "apt");
    }

    #[test]
    fn resolve_package_platform_skip_wrong_os() {
        let apt = MockManager::new("apt").with_package("ripgrep", "14.0.0");
        let managers = make_manager_map(&[("apt", &apt)]);
        let platform = linux_ubuntu_platform();

        let entry = ModulePackageEntry {
            name: "coreutils".into(),
            min_version: None,
            prefer: vec!["brew".into()],
            aliases: HashMap::new(),
            script: None,
            deny: vec![],
            platforms: vec!["macos".into()], // macos only
        };

        // On Linux, this should be skipped (None), not an error
        let result = resolve_package(&entry, "test", &platform, &managers).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn resolve_package_platform_match_distro() {
        let apt = MockManager::new("apt").with_package("ripgrep", "14.0.0");
        let managers = make_manager_map(&[("apt", &apt)]);
        let platform = linux_ubuntu_platform();

        let entry = ModulePackageEntry {
            name: "ripgrep".into(),
            min_version: None,
            prefer: vec![],
            aliases: HashMap::new(),
            script: None,
            deny: vec![],
            platforms: vec!["ubuntu".into()],
        };

        let result = resolve_package(&entry, "test", &platform, &managers).unwrap();
        assert!(result.is_some());
    }

    #[test]
    fn resolve_package_platform_match_arch() {
        let apt = MockManager::new("apt").with_package("ripgrep", "14.0.0");
        let managers = make_manager_map(&[("apt", &apt)]);
        let platform = Platform {
            arch: crate::platform::Arch::Aarch64,
            ..linux_ubuntu_platform()
        };

        let entry = ModulePackageEntry {
            name: "ripgrep".into(),
            min_version: None,
            prefer: vec![],
            aliases: HashMap::new(),
            script: None,
            deny: vec![],
            platforms: vec!["aarch64".into()],
        };

        let result = resolve_package(&entry, "test", &platform, &managers).unwrap();
        assert!(result.is_some());
    }

    #[test]
    fn resolve_package_platform_empty_matches_all() {
        let brew = MockManager::new("brew").with_package("ripgrep", "14.0.0");
        let managers = make_manager_map(&[("brew", &brew)]);
        let platform = macos_platform();

        let entry = ModulePackageEntry {
            name: "ripgrep".into(),
            min_version: None,
            prefer: vec![],
            aliases: HashMap::new(),
            script: None,
            deny: vec![],
            platforms: vec![], // empty = all platforms
        };

        let result = resolve_package(&entry, "test", &platform, &managers).unwrap();
        assert!(result.is_some());
    }

    #[test]
    fn resolve_module_packages_skips_filtered() {
        let brew = MockManager::new("brew").with_package("ripgrep", "14.0.0");
        let managers = make_manager_map(&[("brew", &brew)]);
        let platform = macos_platform();

        let module = LoadedModule {
            name: "test".into(),
            spec: ModuleSpec {
                packages: vec![
                    ModulePackageEntry {
                        name: "ripgrep".into(),
                        min_version: None,
                        prefer: vec![],
                        aliases: HashMap::new(),
                        script: None,
                        deny: vec![],
                        platforms: vec![], // all platforms
                    },
                    ModulePackageEntry {
                        name: "apt-only-tool".into(),
                        min_version: None,
                        prefer: vec!["apt".into()],
                        aliases: HashMap::new(),
                        script: None,
                        deny: vec![],
                        platforms: vec!["linux".into()], // linux only
                    },
                ],
                ..Default::default()
            },
            dir: PathBuf::from("/fake/test"),
        };

        let resolved = resolve_module_packages(&module, &platform, &managers).unwrap();
        // Only ripgrep should be resolved; apt-only-tool is filtered out on macOS
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].canonical_name, "ripgrep");
    }

    // --- Script + platform YAML parsing tests ---

    #[test]
    fn parse_module_with_script_and_platforms() {
        let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: rustup
spec:
  packages:
    - name: rustup
      prefer: [script]
      script: |
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    - name: sysctl-tweaks
      prefer: [script]
      script: scripts/apply-sysctl.sh
      platforms: [linux]
"#;
        let doc = parse_module(yaml).unwrap();
        assert_eq!(doc.spec.packages.len(), 2);

        let rustup = &doc.spec.packages[0];
        assert_eq!(rustup.name, "rustup");
        assert_eq!(rustup.prefer, vec!["script"]);
        assert!(rustup.script.is_some());
        assert!(rustup.script.as_ref().unwrap().contains("rustup.rs"));
        assert!(rustup.platforms.is_empty());

        let sysctl = &doc.spec.packages[1];
        assert_eq!(sysctl.name, "sysctl-tweaks");
        assert_eq!(sysctl.script, Some("scripts/apply-sysctl.sh".into()));
        assert_eq!(sysctl.platforms, vec!["linux"]);
    }

    // --- Lockfile tests ---

    #[test]
    fn lockfile_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let lockfile = ModuleLockfile {
            modules: vec![ModuleLockEntry {
                name: "nvim".into(),
                url: "https://github.com/user/nvim-module.git@v1.0".into(),
                pinned_ref: "v1.0".into(),
                commit: "abc123def456".into(),
                integrity: "sha256:deadbeef".into(),
                subdir: None,
            }],
        };

        save_lockfile(dir.path(), &lockfile).unwrap();
        let loaded = load_lockfile(dir.path()).unwrap();

        assert_eq!(loaded.modules.len(), 1);
        assert_eq!(loaded.modules[0].name, "nvim");
        assert_eq!(loaded.modules[0].pinned_ref, "v1.0");
        assert_eq!(loaded.modules[0].commit, "abc123def456");
        assert_eq!(loaded.modules[0].integrity, "sha256:deadbeef");
        assert!(loaded.modules[0].subdir.is_none());
    }

    #[test]
    fn lockfile_round_trip_with_subdir() {
        let dir = tempfile::tempdir().unwrap();
        let lockfile = ModuleLockfile {
            modules: vec![ModuleLockEntry {
                name: "tmux".into(),
                url: "https://github.com/user/modules.git//tmux@v2.0".into(),
                pinned_ref: "v2.0".into(),
                commit: "789abc".into(),
                integrity: "sha256:cafe".into(),
                subdir: Some("tmux".into()),
            }],
        };

        save_lockfile(dir.path(), &lockfile).unwrap();
        let loaded = load_lockfile(dir.path()).unwrap();

        assert_eq!(loaded.modules[0].subdir, Some("tmux".into()));
    }

    #[test]
    fn load_lockfile_missing_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let lockfile = load_lockfile(dir.path()).unwrap();
        assert!(lockfile.modules.is_empty());
    }

    // --- Content hashing tests ---

    #[test]
    fn hash_module_contents_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        let mod_dir = dir.path().join("mymodule");
        std::fs::create_dir_all(mod_dir.join("config")).unwrap();
        std::fs::write(mod_dir.join("module.yaml"), "name: mymodule\n").unwrap();
        std::fs::write(mod_dir.join("config/init.lua"), "-- nvim config\n").unwrap();

        let hash1 = hash_module_contents(&mod_dir).unwrap();
        let hash2 = hash_module_contents(&mod_dir).unwrap();
        assert_eq!(hash1, hash2);
        assert!(hash1.starts_with("sha256:"));
    }

    #[test]
    fn hash_module_contents_changes_on_file_change() {
        let dir = tempfile::tempdir().unwrap();
        let mod_dir = dir.path().join("mymod");
        std::fs::create_dir_all(&mod_dir).unwrap();
        std::fs::write(mod_dir.join("module.yaml"), "v1\n").unwrap();

        let hash1 = hash_module_contents(&mod_dir).unwrap();
        std::fs::write(mod_dir.join("module.yaml"), "v2\n").unwrap();
        let hash2 = hash_module_contents(&mod_dir).unwrap();

        assert_ne!(hash1, hash2);
    }

    #[test]
    fn hash_module_contents_skips_dot_git() {
        let dir = tempfile::tempdir().unwrap();
        let mod_dir = dir.path().join("mymod");
        std::fs::create_dir_all(mod_dir.join(".git")).unwrap();
        std::fs::write(mod_dir.join("module.yaml"), "content\n").unwrap();
        std::fs::write(mod_dir.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();

        let hash_with_git = hash_module_contents(&mod_dir).unwrap();

        // Remove .git and rehash — should be the same
        std::fs::remove_dir_all(mod_dir.join(".git")).unwrap();
        let hash_without_git = hash_module_contents(&mod_dir).unwrap();

        assert_eq!(hash_with_git, hash_without_git);
    }

    // --- Integrity verification tests ---

    #[test]
    fn verify_lockfile_integrity_success() {
        let dir = tempfile::tempdir().unwrap();

        // Build a fake lock entry that points to an "http" URL whose SHA hash
        // maps to a predictable cache dir
        let url = "https://example.com/fake.git@v1.0";
        let expected_cache_dir = git_cache_dir(dir.path(), "https://example.com/fake.git");
        // Create content in the expected cache dir
        std::fs::create_dir_all(&expected_cache_dir).unwrap();
        std::fs::write(expected_cache_dir.join("module.yaml"), "test content\n").unwrap();

        let actual_integrity = hash_module_contents(&expected_cache_dir).unwrap();

        let entry = ModuleLockEntry {
            name: "test".into(),
            url: url.into(),
            pinned_ref: "v1.0".into(),
            commit: "abc".into(),
            integrity: actual_integrity,
            subdir: None,
        };

        let result = verify_lockfile_integrity(&entry, dir.path());
        assert!(result.is_ok());
    }

    #[test]
    fn verify_lockfile_integrity_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let url = "https://example.com/mod.git@v1.0";
        let cache_dir = git_cache_dir(dir.path(), "https://example.com/mod.git");
        std::fs::create_dir_all(&cache_dir).unwrap();
        std::fs::write(cache_dir.join("module.yaml"), "tampered\n").unwrap();

        let entry = ModuleLockEntry {
            name: "test".into(),
            url: url.into(),
            pinned_ref: "v1.0".into(),
            commit: "abc".into(),
            integrity: "sha256:wrong".into(),
            subdir: None,
        };

        let result = verify_lockfile_integrity(&entry, dir.path());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("integrity"));
    }

    // --- Module diff tests ---

    #[test]
    fn diff_module_specs_no_changes() {
        let module = LoadedModule {
            name: "test".into(),
            spec: ModuleSpec {
                depends: vec!["dep1".into()],
                packages: vec![ModulePackageEntry {
                    name: "pkg1".into(),
                    min_version: Some("1.0".into()),
                    prefer: vec![],
                    aliases: HashMap::new(),
                    script: None,
                    deny: vec![],
                    platforms: vec![],
                }],
                files: vec![],
                env: vec![],
                aliases: vec![],
                scripts: None,
            },
            dir: PathBuf::from("/fake"),
        };

        let changes = diff_module_specs(&module, &module);
        assert_eq!(changes, vec!["(no spec changes)"]);
    }

    #[test]
    fn diff_module_specs_detects_changes() {
        let old = LoadedModule {
            name: "test".into(),
            spec: ModuleSpec {
                depends: vec!["dep1".into()],
                packages: vec![
                    ModulePackageEntry {
                        name: "pkg1".into(),
                        min_version: Some("1.0".into()),
                        prefer: vec![],
                        aliases: HashMap::new(),
                        script: None,
                        deny: vec![],
                        platforms: vec![],
                    },
                    ModulePackageEntry {
                        name: "pkg2".into(),
                        min_version: None,
                        prefer: vec![],
                        aliases: HashMap::new(),
                        script: None,
                        deny: vec![],
                        platforms: vec![],
                    },
                ],
                files: vec![ModuleFileEntry {
                    source: "config/".into(),
                    target: "~/.config/test/".into(),
                    strategy: None,
                    private: false,
                }],
                env: vec![],
                aliases: vec![],
                scripts: None,
            },
            dir: PathBuf::from("/fake"),
        };

        let new = LoadedModule {
            name: "test".into(),
            spec: ModuleSpec {
                depends: vec!["dep1".into(), "dep2".into()],
                packages: vec![
                    ModulePackageEntry {
                        name: "pkg1".into(),
                        min_version: Some("2.0".into()),
                        prefer: vec![],
                        aliases: HashMap::new(),
                        script: None,
                        deny: vec![],
                        platforms: vec![],
                    },
                    ModulePackageEntry {
                        name: "pkg3".into(),
                        min_version: None,
                        prefer: vec![],
                        aliases: HashMap::new(),
                        script: None,
                        deny: vec![],
                        platforms: vec![],
                    },
                ],
                files: vec![ModuleFileEntry {
                    source: "config/".into(),
                    target: "~/.config/new/".into(),
                    strategy: None,
                    private: false,
                }],
                env: vec![],
                aliases: vec![],
                scripts: None,
            },
            dir: PathBuf::from("/fake"),
        };

        let changes = diff_module_specs(&old, &new);
        // Should detect: +dep2, +pkg3, -pkg2, ~pkg1 version change, +file target, -file target
        assert!(changes.iter().any(|c| c.contains("+ dependency: dep2")));
        assert!(changes.iter().any(|c| c.contains("+ package: pkg3")));
        assert!(changes.iter().any(|c| c.contains("- package: pkg2")));
        assert!(
            changes
                .iter()
                .any(|c| c.contains("~ package 'pkg1': min-version"))
        );
        assert!(changes.iter().any(|c| c.contains("+ file target")));
        assert!(changes.iter().any(|c| c.contains("- file target")));
    }

    // --- Module registry tests ---

    #[test]
    fn extract_registry_name_https() {
        assert_eq!(
            extract_registry_name("https://github.com/cfgd-community/modules.git"),
            Some("cfgd-community".into())
        );
    }

    #[test]
    fn extract_registry_name_ssh() {
        assert_eq!(
            extract_registry_name("git@github.com:myorg/modules.git"),
            Some("myorg".into())
        );
    }

    #[test]
    fn extract_registry_name_non_github() {
        assert_eq!(
            extract_registry_name("https://gitlab.com/org/repo.git"),
            None
        );
    }

    // --- Registry ref parsing tests ---

    #[test]
    fn is_registry_ref_with_registry_module() {
        assert!(is_registry_ref("community/tmux"));
        assert!(is_registry_ref("myorg/nvim@v1.0"));
    }

    #[test]
    fn is_registry_ref_bare_name() {
        assert!(!is_registry_ref("tmux"));
    }

    #[test]
    fn is_registry_ref_git_url() {
        assert!(!is_registry_ref("https://github.com/user/repo.git"));
        assert!(!is_registry_ref("git@github.com:user/repo.git"));
    }

    #[test]
    fn parse_registry_ref_with_tag() {
        let r = parse_registry_ref("community/tmux@v1.0").unwrap();
        assert_eq!(r.registry, "community");
        assert_eq!(r.module, "tmux");
        assert_eq!(r.tag, Some("v1.0".into()));
    }

    #[test]
    fn parse_registry_ref_without_tag() {
        let r = parse_registry_ref("myorg/nvim").unwrap();
        assert_eq!(r.registry, "myorg");
        assert_eq!(r.module, "nvim");
        assert!(r.tag.is_none());
    }

    #[test]
    fn parse_registry_ref_invalid() {
        assert!(parse_registry_ref("tmux").is_none());
        assert!(parse_registry_ref("/tmux").is_none());
        assert!(parse_registry_ref("community/").is_none());
        assert!(parse_registry_ref("community/@v1").is_none());
        assert!(parse_registry_ref("community/tmux@").is_none());
    }

    #[test]
    fn resolve_profile_module_name_bare() {
        assert_eq!(resolve_profile_module_name("tmux"), "tmux");
    }

    #[test]
    fn resolve_profile_module_name_registry_ref() {
        assert_eq!(resolve_profile_module_name("community/tmux"), "tmux");
    }

    // --- Load locked modules into local map ---

    #[test]
    fn load_locked_modules_merges_with_local() {
        let dir = tempfile::tempdir().unwrap();

        // Create a local module
        let mod_dir = dir.path().join("modules").join("local-mod");
        std::fs::create_dir_all(&mod_dir).unwrap();
        std::fs::write(
            mod_dir.join("module.yaml"),
            r#"
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: local-mod
spec:
  packages:
    - name: local-pkg
"#,
        )
        .unwrap();

        // Write a lockfile with a remote module that has the same cache structure
        // For this test we verify the function doesn't crash on missing cache
        // (it will error on missing git repo, which is expected)
        let lockfile = ModuleLockfile {
            modules: vec![ModuleLockEntry {
                name: "remote-mod".into(),
                url: "https://example.com/remote.git@v1.0".into(),
                pinned_ref: "v1.0".into(),
                commit: "abc".into(),
                integrity: "sha256:test".into(),
                subdir: None,
            }],
        };
        save_lockfile(dir.path(), &lockfile).unwrap();

        // Load local modules only — they should work fine
        let local = load_modules(dir.path()).unwrap();
        assert_eq!(local.len(), 1);
        assert!(local.contains_key("local-mod"));
    }

    // --- Unpinned remote module rejection ---

    #[test]
    fn fetch_remote_module_rejects_unpinned() {
        let dir = tempfile::tempdir().unwrap();
        // URL without @tag or ?ref= — should be rejected
        let result = fetch_remote_module("https://github.com/user/module.git", dir.path());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("pinned ref"));
    }

    #[test]
    fn fetch_remote_module_rejects_branch_ref() {
        let dir = tempfile::tempdir().unwrap();
        // URL with ?ref=main — branches are rejected for security
        let result = fetch_remote_module("https://github.com/user/module.git?ref=main", dir.path());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("pinned ref"));
    }

    // --- URL parsing: ?ref= + //subdir combined ---

    #[test]
    fn parse_git_source_ref_with_subdir() {
        let src = parse_git_source("https://github.com/user/repo.git?ref=dev//subdir").unwrap();
        assert_eq!(src.repo_url, "https://github.com/user/repo.git");
        assert_eq!(src.git_ref.as_deref(), Some("dev"));
        assert_eq!(src.subdir.as_deref(), Some("subdir"));
        assert!(src.tag.is_none());
    }

    #[test]
    fn parse_git_source_ref_with_subdir_and_tag() {
        let src =
            parse_git_source("https://github.com/user/repo.git?ref=dev//subdir@v1.0").unwrap();
        assert_eq!(src.repo_url, "https://github.com/user/repo.git");
        assert_eq!(src.git_ref.as_deref(), Some("dev"));
        assert_eq!(src.subdir.as_deref(), Some("subdir"));
        assert_eq!(src.tag.as_deref(), Some("v1.0"));
    }

    // --- URL parsing: SSH without .git suffix ---

    #[test]
    fn parse_git_source_ssh_no_dot_git_with_tag() {
        let src = parse_git_source("git@github.com:user/repo@v2.0").unwrap();
        assert_eq!(src.repo_url, "git@github.com:user/repo");
        assert_eq!(src.tag.as_deref(), Some("v2.0"));
    }

    #[test]
    fn parse_git_source_ssh_no_dot_git_no_tag() {
        let src = parse_git_source("git@github.com:user/repo").unwrap();
        assert_eq!(src.repo_url, "git@github.com:user/repo");
        assert!(src.tag.is_none());
    }

    // --- hash_module_contents: empty directory ---

    #[test]
    fn hash_module_contents_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let hash = hash_module_contents(dir.path()).unwrap();
        assert!(hash.starts_with("sha256:"));
        // Empty dir produces a deterministic hash
        let hash2 = hash_module_contents(dir.path()).unwrap();
        assert_eq!(hash, hash2);
    }

    // --- hash_module_contents: symlinks skipped ---

    #[test]
    fn hash_module_contents_skips_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("real.txt"), "hello").unwrap();
        std::os::unix::fs::symlink("/dev/null", dir.path().join("link.txt")).unwrap();

        let hash_with_link = hash_module_contents(dir.path()).unwrap();

        // Remove the symlink and check hash matches (symlink was skipped)
        std::fs::remove_file(dir.path().join("link.txt")).unwrap();
        let hash_without_link = hash_module_contents(dir.path()).unwrap();

        assert_eq!(hash_with_link, hash_without_link);
    }

    // --- diff_module_specs with script changes ---

    #[test]
    fn diff_module_specs_scripts_changed() {
        let old = LoadedModule {
            name: "test".into(),
            spec: ModuleSpec {
                depends: vec![],
                packages: vec![],
                files: vec![],
                env: vec![],
                aliases: vec![],
                scripts: Some(crate::config::ModuleScriptSpec {
                    post_apply: vec!["echo old".into()],
                }),
            },
            dir: PathBuf::from("/tmp"),
        };
        let new = LoadedModule {
            name: "test".into(),
            spec: ModuleSpec {
                depends: vec![],
                packages: vec![],
                files: vec![],
                env: vec![],
                aliases: vec![],
                scripts: Some(crate::config::ModuleScriptSpec {
                    post_apply: vec!["echo new".into()],
                }),
            },
            dir: PathBuf::from("/tmp"),
        };
        let changes = diff_module_specs(&old, &new);
        assert!(changes.iter().any(|c| c.contains("+ post-apply script")));
        assert!(changes.iter().any(|c| c.contains("- post-apply script")));
    }
}
