// Module system — self-contained, portable configuration units
//
// Handles module loading, dependency resolution (topological sort),
// cross-platform package resolution, and git file source management.
//
// Dependency rules: depends on config/, errors/, platform/, providers/ (trait only).
// Must NOT import files/, packages/, secrets/, reconciler/, state/, daemon/.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;

use crate::config::{EnvVar, ModuleSpec, ShellAlias};

mod git;
mod loader;
mod lockfile;
mod registry;
mod resolve;
mod surfaces;

#[cfg(any(test, feature = "test-helpers"))]
pub(crate) use git::set_repo_refresh_ttl_override;
pub use git::{
    GitSource, TagSignatureStatus, check_tag_signature, default_module_cache_dir,
    default_module_cache_dir_for, fetch_git_source, get_head_commit_sha, git_cache_dir,
    is_git_source, parse_git_source,
};
pub use loader::{
    declared_modules_dir, load_module, load_modules, resolve_dependency_order, validate_module_name,
};
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
    fill_available_versions, resolve_module_files, resolve_module_packages, resolve_modules,
    resolve_package,
};
pub(crate) use resolve::{price_package, priceable_manager};
pub use surfaces::{HookScripts, ModuleSurfaces};

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
    /// The version the manager currently offers, when anything asked.
    ///
    /// Resolution fills it only where it had to look anyway — a `minVersion`
    /// constraint. Everywhere else it is a DISPLAY detail and stays `None` until
    /// a surface that renders one calls [`resolve::fill_available_versions`]
    /// (or its survivor-gated planning form,
    /// `Reconciler::fill_planned_versions`); a read path that shows no version
    /// pays no query for it, and neither does a planning path for a package
    /// the machine already holds.
    pub version: Option<String>,
    /// Install script content (inline or file path). Only set when `manager == "script"`.
    pub script: Option<String>,
    /// Idempotency guard: skip the install script if this path already exists.
    /// Only carried for a `prefer: [script]` install (`manager == "script"`).
    pub creates: Option<String>,
    /// Idempotency guard: run the install script only if this command exits zero.
    /// Only carried for a `prefer: [script]` install (`manager == "script"`).
    pub only_if: Option<String>,
    /// Idempotency guard: run the install script only if this command exits
    /// NON-zero. Only carried for a `prefer: [script]` install.
    pub unless: Option<String>,
    /// Whether the module AUTHOR named the manager this entry resolved to.
    ///
    /// `true` when the entry carries a `prefer` list (every candidate then
    /// comes from what the author wrote) or an `aliases` key for the manager
    /// resolution picked. `false` for an entry that named neither: the manager
    /// is then cfgd's own platform default, a choice this crate made and not a
    /// statement by anyone.
    ///
    /// Recorded HERE because the declaration is only in scope at the resolver;
    /// a second walk over the spec to re-derive it is how the two halves drift.
    /// Its one consumer is `reconciler::managers::declared_manager_routes`,
    /// which may only route a provision through a manager the author named —
    /// a defaulted `- name: npm` on a Debian host otherwise became
    /// `provision npm via apt`, pulling apt's whole node toolchain in place of
    /// the brew cascade `plan_managers` documents npm as preferring.
    ///
    /// Not serialized: it is a planner input, and the declaration itself is
    /// already in the module's own spec.
    #[serde(skip)]
    pub manager_declared: bool,
    /// The declared `minVersion` floor, carried through resolution.
    ///
    /// Resolution checks it against what the manager currently OFFERS, which
    /// decides which manager wins; the floor has to survive that so the planner
    /// can ask the second question no name comparison can answer — whether the
    /// copy the machine already HAS clears it. Without it a host holding
    /// `neovim 0.9` under a module declaring `minVersion: 0.11` reads as
    /// converged and the gap is never named. Not serialized: it is a planner
    /// input, and the declared value is already in the module's own spec.
    #[serde(skip)]
    pub min_version: Option<String>,
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
    /// Partial-file merge configuration, present exactly when `strategy` is
    /// `Patch`. A relative `patch.script` resolves against the module's
    /// directory (see `PatchBinding::module`).
    pub patch: Option<crate::config::PatchSpec>,
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
    /// as `subscription.allowScripts || !constraints.noScripts`. When `false`,
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
    pub system: crate::config::SystemSettings,
    pub pre_apply_scripts: Vec<crate::config::ScriptEntry>,
    pub post_apply_scripts: Vec<crate::config::ScriptEntry>,
    pub pre_reconcile_scripts: Vec<crate::config::ScriptEntry>,
    pub post_reconcile_scripts: Vec<crate::config::ScriptEntry>,
    pub on_change_scripts: Vec<crate::config::ScriptEntry>,
    pub on_drift_scripts: Vec<crate::config::ScriptEntry>,
    pub depends: Vec<String>,
    /// Set when nothing REQUESTED this module and a `depends:` pulled it into
    /// the resolution.
    ///
    /// Claimed by the resolver, which is the one place both lists are in hand,
    /// so no surface re-derives it: a `Modules` header row names what the
    /// invocation or the profile declared and annotates what came in behind
    /// them (`Modules  nvim (depends: plugins)`), exactly as the `Profile` row
    /// annotates an `inherits:` chain.
    pub dep_pulled: bool,
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
    /// The six lifecycle hooks paired with the entries this module resolved
    /// for them, in RUN order — the resolved-side mirror of
    /// [`crate::config::ScriptSpec::hooks`], which is the ordering authority
    /// both read from.
    ///
    /// Destructured for the same reason that one is: a seventh hook field does
    /// not compile until it is listed here, so no surface reporting a module's
    /// hooks can silently miss one.
    pub fn script_hooks(&self) -> [(&'static str, &[crate::config::ScriptEntry]); 6] {
        let Self {
            pre_apply_scripts,
            post_apply_scripts,
            pre_reconcile_scripts,
            post_reconcile_scripts,
            on_drift_scripts,
            on_change_scripts,
            name: _,
            packages: _,
            files: _,
            env: _,
            aliases: _,
            system: _,
            depends: _,
            dep_pulled: _,
            dir: _,
            platform_skip_reason: _,
            origin: _,
        } = self;
        [
            ("preApply", pre_apply_scripts),
            ("postApply", post_apply_scripts),
            ("preReconcile", pre_reconcile_scripts),
            ("postReconcile", post_reconcile_scripts),
            ("onDrift", on_drift_scripts),
            ("onChange", on_change_scripts),
        ]
    }

    /// Build a platform-skipped placeholder: identity (`name`, `dir`, `depends`)
    /// is preserved, `platform_skip_reason` is set, and every applyable field
    /// (packages, files, env, aliases, system, scripts) is empty. Centralizing
    /// the empty-contents invariant here keeps a skipped module from silently
    /// acquiring applyable state if `ResolvedModule` later gains a field.
    pub fn skipped(
        name: String,
        dir: PathBuf,
        depends: Vec<String>,
        dep_pulled: bool,
        reason: String,
        origin: Option<String>,
    ) -> Self {
        ResolvedModule {
            name,
            packages: Vec::new(),
            files: Vec::new(),
            env: Vec::new(),
            aliases: Vec::new(),
            system: BTreeMap::new(),
            pre_apply_scripts: Vec::new(),
            post_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: Vec::new(),
            on_drift_scripts: Vec::new(),
            depends,
            dep_pulled,
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
    /// `metadata.version` as authored, when the module declares one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Provenance: `None` = consumer-local (or locked/registry) module;
    /// `Some(source_name)` = body delivered by the named ConfigSource.
    pub origin: Option<String>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
