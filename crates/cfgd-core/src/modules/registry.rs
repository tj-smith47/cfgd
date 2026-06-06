//! Module registries — git repos with prescribed directory structure, plus
//! `registry/module[@tag]` reference parsing and remote-module fetching.

use std::collections::HashMap;
use std::path::Path;

use crate::config::ModuleRegistryEntry;
use crate::errors::{ModuleError, Result};

use super::LoadedModule;
use super::git::{
    GitSource, clone_repo, fetch_existing_repo, fetch_git_source, get_head_commit_sha,
    git_cache_dir, is_git_source, open_repo, parse_git_source,
};
use super::loader::load_module;
use super::lockfile::hash_module_contents;

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
pub fn fetch_remote_module(
    url: &str,
    cache_base: &Path,
    printer: &crate::output::Printer,
) -> Result<FetchedRemoteModule> {
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

    let local_path = fetch_git_source(&git_src, cache_base, "remote", printer)?;

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
    printer: &crate::output::Printer,
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
        fetch_existing_repo(&cache_dir, &git_src, &registry.name, printer)?;
    } else {
        clone_repo(&cache_dir, &git_src, &registry.name, printer)?;
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
    let tag_names = repo
        .tag_names(None)
        .map_err(|e| ModuleError::GitFetchFailed {
            module: source_name.to_string(),
            url: String::new(),
            message: format!("cannot list tags: {e}"),
        })?;
    Ok(group_module_tags(tag_names.iter().flatten()))
}

/// Group git tag names that follow the `<module>/<version>` convention into a
/// `HashMap<module, sorted versions>`. Tags without a `/` (or without anything
/// after the first `/`) are silently dropped — the registry layout requires
/// the prefix, so anything else is unrelated to module versioning.
///
/// Each module's tag list is sorted with `parse_loose_version` (best-effort
/// semver) and falls back to lexicographic string compare for tags that
/// don't parse as semver. The last element is therefore the highest version
/// — matching the consumer expectation in `latest_module_version`.
pub(super) fn group_module_tags<'a, I>(tag_names: I) -> HashMap<String, Vec<String>>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut result: HashMap<String, Vec<String>> = HashMap::new();
    for tag_name in tag_names {
        if let Some((module, version)) = tag_name.split_once('/') {
            result
                .entry(module.to_string())
                .or_default()
                .push(version.to_string());
        }
    }
    for tags in result.values_mut() {
        tags.sort_by(|a, b| {
            // `parse_loose_version` strips a leading `v` itself, so the
            // registry convention `<module>/v<X.Y.Z>` sorts correctly here.
            let av = crate::parse_loose_version(a);
            let bv = crate::parse_loose_version(b);
            match (av, bv) {
                (Some(av), Some(bv)) => av.cmp(&bv),
                _ => a.cmp(b),
            }
        });
    }
    result
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

/// Resolve the latest published version tag for `module_name` directly against
/// a remote repo, without relying on a local cache being fully populated.
///
/// Module versions are published as git tags named `<module>/<version>` (e.g.
/// `tmux/v2.0.0`). The version part is sorted with [`group_module_tags`]
/// (loose-semver), so the returned value is the highest version (e.g.
/// `v2.0.0`). Returns `Ok(None)` when no `<module>/<version>` tag exists for
/// the module.
///
/// Unlike [`latest_module_version`], this lists tags over the network via
/// `git ls-remote --tags` and therefore sees every tag regardless of how the
/// install-time cache was cloned (a shallow single-tag clone hides the rest).
/// The `repo_url` is attacker-influenced (it comes from the lockfile, which
/// records a remote source), so the call is hardened the same way the
/// `pinVersion` resolver hardens its `ls-remote`: `git_cmd_safe`
/// (`GIT_TERMINAL_PROMPT=0`, no system/global config, no credential helpers),
/// `--end-of-options` before the URL, and a bounded timeout.
pub fn latest_module_version_remote(repo_url: &str, module_name: &str) -> Result<Option<String>> {
    let mut cmd = crate::git_cmd_safe(Some(repo_url), None);
    cmd.args(["ls-remote", "--tags", "--end-of-options", repo_url]);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let output =
        crate::command_output_with_timeout(&mut cmd, crate::GIT_NETWORK_TIMEOUT).map_err(|e| {
            ModuleError::GitFetchFailed {
                module: module_name.to_string(),
                url: repo_url.to_string(),
                message: format!("ls-remote --tags failed: {e}"),
            }
        })?;
    if !output.status.success() {
        return Err(ModuleError::GitFetchFailed {
            module: module_name.to_string(),
            url: repo_url.to_string(),
            message: format!(
                "ls-remote --tags failed: {}",
                crate::stderr_lossy_trimmed(&output)
            ),
        }
        .into());
    }

    let stdout = crate::stdout_lossy_trimmed(&output);
    let grouped = group_module_tags(parse_ls_remote_tag_names(&stdout));
    Ok(grouped.get(module_name).and_then(|t| t.last()).cloned())
}

/// Extract `<module>/<version>` tag names from `git ls-remote --tags` stdout.
///
/// Each line is `<sha>\t<refname>`. The `refs/tags/` prefix is stripped and the
/// peeled-tag lines (`refs/tags/<name>^{}`) are dropped so an annotated tag is
/// not double-counted. The returned names are fed to [`group_module_tags`],
/// which keeps only those matching the `<module>/<version>` layout.
pub(super) fn parse_ls_remote_tag_names(stdout: &str) -> Vec<&str> {
    stdout
        .lines()
        .filter_map(|line| line.split('\t').nth(1))
        .filter_map(|refname| refname.strip_prefix("refs/tags/"))
        .filter(|name| !name.ends_with("^{}"))
        .collect()
}
