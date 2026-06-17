//! Single source of truth for "effective desired state" — the profile config
//! deep-merged with the modules it pulls in.
//!
//! cfgd's write path and its several read-back surfaces (verify, diff, status,
//! compliance) each independently combine a profile with its modules; when those
//! combinations disagree, module resources become invisible to some commands.
//! The derivations here are the one place that computes the combined view, so
//! every path applies the same merge and the same cross-scope dedup *rules*.
//!
//! These functions are **host-agnostic**: they return what `profile ⊕ modules`
//! desires, not what the current host can act on. Callers that act on the result
//! (apply, verify, diff) intersect it with [`crate::providers::ProviderRegistry`]
//! availability — e.g. iterating `available_package_managers()` — exactly as the
//! write path does. Keeping availability out of the derivation is what makes the
//! write and read paths agree on the desired set.

use std::collections::HashMap;
use std::path::Path;

use crate::config::{ManagedFileSpec, MergedProfile, PackageClaim, desired_packages_for_spec};
use crate::modules::ResolvedModule;
use crate::to_posix_string;

/// Where a resource in the effective desired state originated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// Declared directly in the profile (`spec.files` / `spec.packages`).
    ///
    /// Source attribution finer than profile-vs-module (e.g. which config source
    /// a multi-source profile entry came from) is future work.
    Profile,
    /// Contributed by a module; the inner value is the module name.
    Module(String),
}

/// A single desired package after the profile and its modules are combined and
/// cross-scope deduplicated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectivePackage {
    /// Package manager that will install it (e.g. `brew`, `apt`, `cargo`).
    pub manager: String,
    /// Package name as the manager will resolve it.
    pub name: String,
    /// Whether the package came from the profile or a specific module.
    pub origin: Origin,
}

/// A single managed file after the profile and its modules are combined.
///
/// Identity is keyed on [`target`](Self::target): a file is the same managed
/// file regardless of which scope declared it.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveFile {
    /// Resolved absolute source path. Profile sources (written relative in
    /// config) are resolved against `config_dir`; module sources are already
    /// absolute and pass through. Consumers can content-check directly without
    /// re-resolving.
    pub source: String,
    /// Target path on the machine. The identity key for a managed file.
    pub target: std::path::PathBuf,
    /// Per-file deployment strategy override, if any.
    pub strategy: Option<crate::config::FileStrategy>,
    /// Unix permission bits to apply after deployment (e.g. `"600"`), if any.
    pub permissions: Option<String>,
    /// Encryption settings carried with the file, if any.
    pub encryption: Option<crate::config::EncryptionSpec>,
    /// Local-only source: skipped where it doesn't exist rather than reported
    /// missing. Always `false` for module files.
    pub private: bool,
    /// Whether the source was fetched from git. Always `false` for profile files.
    pub is_git_source: bool,
    /// Whether the file came from the profile or a specific module.
    pub origin: Origin,
    /// Tera template origin used to render the source: the profile entry's
    /// `origin` for profile files, `None` for module files (module sources carry
    /// no tera origin). Drives content rendering so a profile template compares
    /// against its correctly-rendered bytes.
    pub tera_origin: Option<String>,
}

/// Build the effective system-configurator map: start from the profile's system
/// settings, then deep-merge each module's system settings in module order.
///
/// Modules override the profile at leaf level, and a later module overrides an
/// earlier one — consistent with how env and aliases merge.
pub fn effective_system_map(
    profile: &MergedProfile,
    modules: &[ResolvedModule],
) -> HashMap<String, serde_yaml::Value> {
    let mut system = profile.system.clone();
    for module in modules {
        for (key, value) in &module.system {
            crate::deep_merge_yaml(
                system.entry(key.clone()).or_insert(serde_yaml::Value::Null),
                value,
            );
        }
    }
    system
}

/// Build the effective desired package set: the profile's packages combined with
/// every module's packages, cross-scope deduplicated by the shared
/// [`PackageClaim`] rules (a `(manager, name)` declared in both a module and the
/// profile, or in two modules, appears once).
///
/// Module installs win over profile duplicates, and among modules the earlier
/// one wins. Custom inline `script` "packages" are never deduplicated — two
/// same-named scripts may differ and both are kept. Each surviving entry carries
/// its origin so a consumer can attribute it to the profile or a module.
///
/// The result lists every *configured* manager's packages and is host-agnostic;
/// callers intersect it with registry availability (see the module docs).
pub fn effective_desired_packages(
    profile: &MergedProfile,
    modules: &[ResolvedModule],
) -> Vec<EffectivePackage> {
    // Drive the shared claiming primitive directly so the rules (module wins,
    // earlier-module wins, `script` exempt) stay in one implementation without
    // reconstructing the reconciler's Action shapes.
    let mut claim = PackageClaim::new();
    let mut packages = Vec::new();

    for module in modules {
        for pkg in &module.packages {
            if claim.claim_module(&pkg.manager, &pkg.resolved_name) {
                packages.push(EffectivePackage {
                    manager: pkg.manager.clone(),
                    name: pkg.resolved_name.clone(),
                    origin: Origin::Module(module.name.clone()),
                });
            }
        }
    }

    for manager in profile.packages.manager_names() {
        for name in desired_packages_for_spec(&manager, &profile.packages) {
            if !claim.is_claimed(&manager, &name) {
                packages.push(EffectivePackage {
                    manager: manager.clone(),
                    name,
                    origin: Origin::Profile,
                });
            }
        }
    }

    packages
}

/// Build the effective managed-file list: every file declared by the profile
/// (`spec.files.managed`) followed by every file each module deploys, each
/// tagged with its origin so a consumer can build a stable resource id.
///
/// Profile sources are resolved against `config_dir` so [`EffectiveFile::source`]
/// is always an absolute path a consumer can content-check without re-resolving;
/// module sources are already absolute and pass through. A profile source that
/// fails traversal validation is kept as its raw relative string rather than
/// dropped, so the file still appears (a downstream content check then reports it
/// as a missing/unresolvable source rather than silently hiding the entry).
pub fn effective_files(
    profile: &MergedProfile,
    modules: &[ResolvedModule],
    config_dir: &Path,
) -> Vec<EffectiveFile> {
    let mut files = Vec::new();

    for spec in &profile.files.managed {
        files.push(profile_file(spec, config_dir));
    }

    for module in modules {
        for file in &module.files {
            files.push(EffectiveFile {
                source: to_posix_string(&file.source),
                target: file.target.clone(),
                strategy: file.strategy,
                permissions: file.permissions.clone(),
                encryption: file.encryption.clone(),
                private: false,
                is_git_source: file.is_git_source,
                origin: Origin::Module(module.name.clone()),
                tera_origin: None,
            });
        }
    }

    files
}

fn profile_file(spec: &ManagedFileSpec, config_dir: &Path) -> EffectiveFile {
    let resolved_source = crate::resolve_relative_path(Path::new(&spec.source), config_dir)
        .map_or_else(|_| spec.source.clone(), |p| to_posix_string(&p));
    EffectiveFile {
        source: resolved_source,
        target: spec.target.clone(),
        strategy: spec.strategy,
        permissions: spec.permissions.clone(),
        encryption: spec.encryption.clone(),
        private: spec.private,
        is_git_source: false,
        origin: Origin::Profile,
        tera_origin: spec.origin.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        EncryptionMode, EncryptionSpec, FileStrategy, FilesSpec, ManagedFileSpec, PackagesSpec,
    };
    use crate::modules::{ResolvedFile, ResolvedModule, ResolvedPackage};
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn empty_profile() -> MergedProfile {
        MergedProfile {
            modules: Vec::new(),
            env: Vec::new(),
            env_scope: crate::config::EnvScope::All,
            aliases: Vec::new(),
            packages: PackagesSpec::default(),
            files: FilesSpec::default(),
            system: HashMap::new(),
            secrets: Vec::new(),
            scripts: crate::config::ScriptSpec::default(),
        }
    }

    fn module(name: &str) -> ResolvedModule {
        ResolvedModule {
            name: name.to_string(),
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
            depends: Vec::new(),
            dir: PathBuf::from("/tmp/module"),
            platform_skip_reason: None,
            origin: None,
        }
    }

    fn pkg(manager: &str, name: &str) -> ResolvedPackage {
        ResolvedPackage {
            canonical_name: name.to_string(),
            resolved_name: name.to_string(),
            manager: manager.to_string(),
            version: None,
            script: None,
            creates: None,
            only_if: None,
            unless: None,
        }
    }

    fn yaml(value: &str) -> serde_yaml::Value {
        serde_yaml::from_str(value).expect("valid yaml fixture")
    }

    // --- effective_system_map ------------------------------------------------

    #[test]
    fn system_map_profile_only() {
        let mut profile = empty_profile();
        profile.system.insert("shell".into(), yaml("default: zsh"));

        let map = effective_system_map(&profile, &[]);

        assert_eq!(map.get("shell"), Some(&yaml("default: zsh")));
    }

    #[test]
    fn system_map_module_only() {
        let profile = empty_profile();
        let mut m = module("dev");
        m.system.insert("sysctl".into(), yaml("vm.swappiness: 10"));

        let map = effective_system_map(&profile, &[m]);

        assert_eq!(map.get("sysctl"), Some(&yaml("vm.swappiness: 10")));
    }

    #[test]
    fn system_map_module_overrides_profile_leaf() {
        let mut profile = empty_profile();
        profile
            .system
            .insert("shell".into(), yaml("default: bash\npath: /usr/bin"));
        let mut m = module("dev");
        m.system.insert("shell".into(), yaml("default: zsh"));

        let map = effective_system_map(&profile, &[m]);

        // Module wins at the overlapping leaf; non-overlapping profile leaf survives.
        assert_eq!(
            map.get("shell"),
            Some(&yaml("default: zsh\npath: /usr/bin"))
        );
    }

    #[test]
    fn system_map_later_module_overrides_earlier_module_leaf() {
        let mut profile = empty_profile();
        profile.system.insert("shell".into(), yaml("default: bash"));
        let mut a = module("a");
        a.system.insert("shell".into(), yaml("default: zsh\nx: 1"));
        let mut b = module("b");
        b.system.insert("shell".into(), yaml("default: fish"));

        let map = effective_system_map(&profile, &[a, b]);

        // Three-way ordering: profile < module a < module b. The later module
        // wins at the overlapping leaf; the earlier module's non-overlapping
        // leaf survives.
        assert_eq!(map.get("shell"), Some(&yaml("default: fish\nx: 1")));
    }

    // --- effective_desired_packages ------------------------------------------

    #[test]
    fn packages_profile_only() {
        let mut profile = empty_profile();
        profile.packages.dnf = vec!["ripgrep".into(), "fd".into()];

        let pkgs = effective_desired_packages(&profile, &[]);

        assert_eq!(pkgs.len(), 2);
        assert!(pkgs.iter().all(|p| p.manager == "dnf"));
        assert!(pkgs.iter().all(|p| p.origin == Origin::Profile));
        assert!(pkgs.iter().any(|p| p.name == "ripgrep"));
        assert!(pkgs.iter().any(|p| p.name == "fd"));
    }

    #[test]
    fn packages_module_only() {
        let profile = empty_profile();
        let mut m = module("dev");
        m.packages = vec![pkg("cargo", "ripgrep")];

        let pkgs = effective_desired_packages(&profile, &[m]);

        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].manager, "cargo");
        assert_eq!(pkgs[0].name, "ripgrep");
        assert_eq!(pkgs[0].origin, Origin::Module("dev".into()));
    }

    #[test]
    fn packages_duplicate_deduped_module_wins() {
        let mut profile = empty_profile();
        profile.packages.dnf = vec!["ripgrep".into()];
        let mut m = module("dev");
        m.packages = vec![pkg("dnf", "ripgrep")];

        let pkgs = effective_desired_packages(&profile, &[m]);

        // Same (manager, name) in both: one entry, attributed to the module.
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].manager, "dnf");
        assert_eq!(pkgs[0].name, "ripgrep");
        assert_eq!(pkgs[0].origin, Origin::Module("dev".into()));
    }

    #[test]
    fn packages_different_managers_kept_separate() {
        let mut profile = empty_profile();
        profile.packages.dnf = vec!["ripgrep".into()];
        let mut m = module("dev");
        m.packages = vec![pkg("cargo", "ripgrep")];

        let pkgs = effective_desired_packages(&profile, &[m]);

        // Same name, different manager: both kept.
        assert_eq!(pkgs.len(), 2);
        assert!(
            pkgs.iter()
                .any(|p| p.manager == "cargo" && p.origin == Origin::Module("dev".into()))
        );
        assert!(
            pkgs.iter()
                .any(|p| p.manager == "dnf" && p.origin == Origin::Profile)
        );
    }

    #[test]
    fn packages_script_manager_duplicates_both_survive() {
        let profile = empty_profile();
        let mut a = module("a");
        a.packages = vec![pkg("script", "setup")];
        let mut b = module("b");
        b.packages = vec![pkg("script", "setup")];

        let pkgs = effective_desired_packages(&profile, &[a, b]);

        // script is exempt from dedup: both same-named scripts survive, each
        // attributed to its own module.
        assert_eq!(pkgs.len(), 2);
        assert!(
            pkgs.iter()
                .all(|p| p.manager == "script" && p.name == "setup")
        );
        assert!(pkgs.iter().any(|p| p.origin == Origin::Module("a".into())));
        assert!(pkgs.iter().any(|p| p.origin == Origin::Module("b".into())));
    }

    #[test]
    fn packages_module_vs_module_earlier_wins() {
        let profile = empty_profile();
        let mut a = module("a");
        a.packages = vec![pkg("brew", "fd")];
        let mut b = module("b");
        b.packages = vec![pkg("brew", "fd")];

        let pkgs = effective_desired_packages(&profile, &[a, b]);

        // Same (manager, name) in two modules: one entry, attributed to the
        // earlier module in slice order.
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].manager, "brew");
        assert_eq!(pkgs[0].name, "fd");
        assert_eq!(pkgs[0].origin, Origin::Module("a".into()));
    }

    // --- effective_files -----------------------------------------------------

    fn managed(source: &str, target: &str) -> ManagedFileSpec {
        ManagedFileSpec {
            source: source.to_string(),
            target: PathBuf::from(target),
            strategy: Some(FileStrategy::Copy),
            private: false,
            origin: None,
            encryption: Some(EncryptionSpec {
                backend: "sops".into(),
                mode: EncryptionMode::InRepo,
            }),
            permissions: Some("600".into()),
        }
    }

    fn resolved_file(source: &str, target: &str) -> ResolvedFile {
        ResolvedFile {
            source: PathBuf::from(source),
            target: PathBuf::from(target),
            is_git_source: true,
            strategy: Some(FileStrategy::Symlink),
            encryption: None,
            permissions: Some("644".into()),
        }
    }

    #[test]
    fn files_profile_only() {
        let mut profile = empty_profile();
        let mut spec = managed("dot/gitconfig", "~/.gitconfig");
        spec.private = true;
        spec.origin = Some("local".into());
        profile.files.managed = vec![spec];

        let config_dir = PathBuf::from("/cfg");
        let files = effective_files(&profile, &[], &config_dir);

        assert_eq!(files.len(), 1);
        // Profile source resolved against config_dir to an absolute path.
        assert_eq!(files[0].source, "/cfg/dot/gitconfig");
        assert_eq!(files[0].target, PathBuf::from("~/.gitconfig"));
        assert_eq!(files[0].strategy, Some(FileStrategy::Copy));
        assert_eq!(files[0].permissions.as_deref(), Some("600"));
        assert!(files[0].encryption.is_some());
        // Profile carries `private`; git-source is never set for profile files.
        assert!(files[0].private);
        assert!(!files[0].is_git_source);
        assert_eq!(files[0].origin, Origin::Profile);
        // Profile files carry the tera origin from the spec.
        assert_eq!(files[0].tera_origin.as_deref(), Some("local"));
    }

    #[test]
    fn files_module_only() {
        let profile = empty_profile();
        let mut m = module("dev");
        m.files = vec![resolved_file("/cache/vimrc", "~/.vimrc")];

        let files = effective_files(&profile, &[m], Path::new("/cfg"));

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].source, "/cache/vimrc");
        assert_eq!(files[0].target, PathBuf::from("~/.vimrc"));
        assert_eq!(files[0].strategy, Some(FileStrategy::Symlink));
        assert_eq!(files[0].permissions.as_deref(), Some("644"));
        // Module carries `is_git_source`; `private` is never set for module files.
        assert!(files[0].is_git_source);
        assert!(!files[0].private);
        assert_eq!(files[0].origin, Origin::Module("dev".into()));
        // Module files carry no tera origin.
        assert_eq!(files[0].tera_origin, None);
    }

    #[test]
    fn files_profile_and_module_combined() {
        let mut profile = empty_profile();
        profile.files.managed = vec![managed("dot/gitconfig", "~/.gitconfig")];
        let mut m = module("dev");
        m.files = vec![resolved_file("/cache/vimrc", "~/.vimrc")];

        let files = effective_files(&profile, &[m], Path::new("/cfg"));

        assert_eq!(files.len(), 2);
        assert_eq!(files[0].origin, Origin::Profile);
        assert_eq!(files[1].origin, Origin::Module("dev".into()));
    }
}
