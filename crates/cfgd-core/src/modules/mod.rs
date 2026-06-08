// Module system — self-contained, portable configuration units
//
// Handles module loading, dependency resolution (topological sort),
// cross-platform package resolution, and git file source management.
//
// Dependency rules: depends on config/, errors/, platform/, providers/ (trait only).
// Must NOT import files/, packages/, secrets/, reconciler/, state/, daemon/.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Serialize;

use crate::config::{EnvVar, ModuleSpec, ShellAlias};

mod git;
mod loader;
mod lockfile;
mod registry;
mod resolve;

pub use git::{
    GitSource, TagSignatureStatus, check_tag_signature, default_module_cache_dir, fetch_git_source,
    get_head_commit_sha, git_cache_dir, is_git_source, parse_git_source,
};
pub use loader::{load_module, load_modules, resolve_dependency_order};
pub use lockfile::{
    diff_module_specs, hash_module_contents, load_all_modules, load_locked_modules, load_lockfile,
    load_source_modules, save_lockfile, verify_lockfile_integrity,
};
pub use registry::{
    FetchedRemoteModule, RegistryModule, RegistryRef, extract_registry_name,
    fetch_registry_modules, fetch_remote_module, is_registry_ref, latest_module_version,
    latest_module_version_remote, parse_registry_ref, resolve_profile_module_name,
};
pub use resolve::{
    resolve_module_files, resolve_module_packages, resolve_modules, resolve_package,
};

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
    /// Encryption settings carried from the module file entry.
    pub encryption: Option<crate::config::EncryptionSpec>,
    /// Unix permission bits (e.g. "600", "644") to apply after deployment.
    pub permissions: Option<String>,
}

/// A root of source-delivered module bodies, derived from a subscribed
/// ConfigSource's cache. `offered` is the publisher-declared allow-list
/// (`provides.modules` in the source manifest); only names in `offered` whose
/// body exists under `modules_dir/<name>/module.yaml` are eligible to load.
/// Higher `priority` wins among sources; consumer-local modules always win.
#[derive(Debug, Clone)]
pub struct SourceModuleRoot {
    pub source_name: String,
    pub priority: u32,
    pub modules_dir: PathBuf,
    pub offered: Vec<String>,
    /// Whether this source is permitted to deliver lifecycle scripts and
    /// `prefer: [script]` package installs through its module bodies. Computed
    /// as `subscription.allowScripts || !constraints.no_scripts`. When `false`,
    /// loading a source-delivered body that carries any script is FATAL
    /// ([`ModuleError::ScriptsNotAllowed`](crate::errors::ModuleError::ScriptsNotAllowed)).
    pub scripts_permitted: bool,
}

/// A fully resolved module — ready for the reconciler.
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedModule {
    pub name: String,
    pub packages: Vec<ResolvedPackage>,
    pub files: Vec<ResolvedFile>,
    pub env: Vec<EnvVar>,
    pub aliases: Vec<ShellAlias>,
    /// System configurator settings declared by this module.
    /// Deep-merged into the profile system map during reconciliation; module wins on conflict.
    pub system: HashMap<String, serde_yaml::Value>,
    pub pre_apply_scripts: Vec<crate::config::ScriptEntry>,
    pub post_apply_scripts: Vec<crate::config::ScriptEntry>,
    pub pre_reconcile_scripts: Vec<crate::config::ScriptEntry>,
    pub post_reconcile_scripts: Vec<crate::config::ScriptEntry>,
    pub on_change_scripts: Vec<crate::config::ScriptEntry>,
    pub on_drift_scripts: Vec<crate::config::ScriptEntry>,
    pub depends: Vec<String>,
    /// Module directory — used as working directory for module scripts.
    pub dir: PathBuf,
    /// Set when the module is gated out by its `spec.platforms` on the current
    /// platform. A skipped module carries empty packages/files/scripts and is
    /// surfaced as a visible Skip action (never silently dropped).
    pub platform_skip_reason: Option<String>,
    /// Provenance: `None` = consumer-local (or locked/registry) module;
    /// `Some(source_name)` = body delivered by the named ConfigSource.
    pub origin: Option<String>,
}

impl ResolvedModule {
    /// Build a platform-skipped placeholder: identity (`name`, `dir`, `depends`)
    /// is preserved, `platform_skip_reason` is set, and every applyable field
    /// (packages, files, env, aliases, system, scripts) is empty. Centralizing
    /// the empty-contents invariant here keeps a skipped module from silently
    /// acquiring applyable state if `ResolvedModule` later gains a field.
    pub fn skipped(
        name: String,
        dir: PathBuf,
        depends: Vec<String>,
        reason: String,
        origin: Option<String>,
    ) -> Self {
        ResolvedModule {
            name,
            packages: Vec::new(),
            files: Vec::new(),
            env: Vec::new(),
            aliases: Vec::new(),
            system: HashMap::new(),
            pre_apply_scripts: Vec::new(),
            post_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: Vec::new(),
            on_drift_scripts: Vec::new(),
            depends,
            dir,
            platform_skip_reason: Some(reason),
            origin,
        }
    }
}

// ---------------------------------------------------------------------------
// Loaded module — parsed from YAML but not yet resolved
// ---------------------------------------------------------------------------

/// A module loaded from disk.
#[derive(Debug, Clone, Serialize)]
pub struct LoadedModule {
    pub name: String,
    pub spec: ModuleSpec,
    pub dir: PathBuf,
    /// Provenance: `None` = consumer-local (or locked/registry) module;
    /// `Some(source_name)` = body delivered by the named ConfigSource.
    pub origin: Option<String>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
