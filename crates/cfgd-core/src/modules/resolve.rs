//! Package and file resolution — turn LoadedModules into ResolvedModules.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use std::collections::HashSet;

use crate::config::ModulePackageEntry;
use crate::errors::{ModuleError, Result};
use crate::platform::Platform;
use crate::providers::{PackageManager, PackageManagerExt};

use crate::errors::CfgdError;

use super::git::{fetch_git_source, is_git_source, parse_git_source};
use super::loader::resolve_dependency_order;
use super::lockfile::load_all_modules;
use super::registry::resolve_profile_module_name;
use super::{LoadedModule, ResolvedFile, ResolvedModule, ResolvedPackage, SourceModuleRoot};

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
                creates: entry.creates.clone(),
                only_if: entry.only_if.clone(),
                unless: entry.unless.clone(),
                min_version: entry.min_version.clone(),
            }));
        }

        let mgr = match managers.get(candidate.as_str()) {
            Some(m) => *m,
            None => continue,
        };

        // Asked once: `is_available()` is a PATH probe, and this runs per
        // candidate manager of every declared package.
        let available = mgr.is_available();
        let bootstrappable = !available && mgr.can_bootstrap();
        if !available && !bootstrappable {
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
                creates: None,
                only_if: None,
                unless: None,
                min_version: entry.min_version.clone(),
            }));
        }

        if let Some(ref min_ver) = entry.min_version {
            match mgr.available_version_memoized(&resolved_name) {
                Ok(Some(ver)) => {
                    // Manager-aware: pkg (FreeBSD) versions are not semver, so the
                    // manager compares against its own scheme; everyone else falls
                    // through to the loose-semver default.
                    if !mgr.version_meets_minimum(&ver, min_ver) {
                        continue;
                    }
                    return Ok(Some(ResolvedPackage {
                        canonical_name: entry.name.clone(),
                        resolved_name,
                        manager: candidate.clone(),
                        version: Some(ver),
                        script: None,
                        creates: None,
                        only_if: None,
                        unless: None,
                        min_version: entry.min_version.clone(),
                    }));
                }
                Ok(None) => continue,
                Err(_) => continue,
            }
        } else {
            // No min-version: first available manager wins, and nothing about
            // that choice depends on what the manager currently offers — so the
            // version query is left to `fill_available_versions`, which the
            // paths that DISPLAY a version call and the read paths do not.
            return Ok(Some(ResolvedPackage {
                canonical_name: entry.name.clone(),
                resolved_name,
                manager: candidate.clone(),
                version: None,
                script: None,
                creates: None,
                only_if: None,
                unless: None,
                min_version: entry.min_version.clone(),
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

/// Price every resolved package that does not already carry a version, so a
/// surface that RENDERS one has it.
///
/// Resolution itself no longer asks: a package with no `minVersion` is placed on
/// the first available manager whatever that manager currently offers, so the
/// query answered nothing about the outcome and was paid by `status`, `diff`,
/// `verify`, `compliance`, `checkin` and `decide` — none of which show a version
/// — once per declared package, on every invocation and every daemon tick.
///
/// Call it from the paths that consume the version and from no others. Two
/// surfaces do: `cfgd doctor` and `cfgd module show`, which print a version
/// per DECLARED package without planning. Every PLANNING path — `cfgd apply`,
/// `cfgd plan`, the daemon's reconcile tick, both `cfgd init` apply paths,
/// and `cfgd module create --apply` — routes through
/// [`crate::reconciler::Reconciler::fill_planned_versions`] instead, the
/// survivor-gated form of this fill: a package the machine already holds is
/// elided from the plan, so it renders and stores nothing and its version
/// query buys nothing, while a package that does get planned is priced
/// through the same memoized query and renders byte-identically.
///
/// The gating reproduces what resolution used to do exactly, so those surfaces
/// render byte-identically: a package already carrying a version (the
/// `minVersion` check found one) is left alone, `script` packages have no
/// manager to ask, and a manager that is not available is not asked — an
/// unavailable-but-bootstrappable manager resolved optimistically with no
/// version before this existed and still does.
pub fn fill_available_versions(
    packages: &mut [ResolvedPackage],
    managers: &HashMap<String, &dyn PackageManager>,
) {
    for pkg in packages {
        let Some(mgr) = priceable_manager(pkg, managers) else {
            continue;
        };
        price_package(pkg, mgr);
    }
}

/// The manager `pkg` can be priced through, when pricing applies at all:
/// `None` for a package already carrying a version (the `minVersion` check
/// found one), a `script` package (no manager to ask), an unregistered
/// manager, or one that is not available (a bootstrappable manager resolves
/// optimistically with no version). The ONE askability gate both
/// [`fill_available_versions`] and
/// [`crate::reconciler::Reconciler::fill_planned_versions`] read, so the
/// survivor gate stays their only difference.
pub(crate) fn priceable_manager<'m>(
    pkg: &ResolvedPackage,
    managers: &HashMap<String, &'m dyn PackageManager>,
) -> Option<&'m dyn PackageManager> {
    if pkg.version.is_some() || pkg.manager == "script" {
        return None;
    }
    let mgr = *managers.get(pkg.manager.as_str())?;
    mgr.is_available().then_some(mgr)
}

/// Price `pkg` through `mgr`'s memoized offer. A manager that cannot answer
/// is not a failure — the version is a display detail, and the package still
/// installs.
pub(crate) fn price_package(pkg: &mut ResolvedPackage, mgr: &dyn PackageManager) {
    pkg.version = mgr
        .available_version_memoized(&pkg.resolved_name)
        .ok()
        .flatten();
}

// ---------------------------------------------------------------------------
// File resolution
// ---------------------------------------------------------------------------

/// Resolve module file entries to concrete local paths.
/// Local sources are resolved relative to the module directory.
/// Git sources are cloned/fetched to cache and resolved to the local cache path.
pub fn resolve_module_files(
    module: &LoadedModule,
    cache_base: &Path,
    printer: &crate::output::Printer,
) -> Result<Vec<ResolvedFile>> {
    let mut resolved = Vec::new();

    for entry in &module.spec.files {
        if is_git_source(&entry.source) {
            let git_src = parse_git_source(&entry.source)?;
            let local_path = fetch_git_source(&git_src, cache_base, &module.name, printer)?;

            resolved.push(ResolvedFile {
                source: local_path,
                target: crate::expand_tilde(Path::new(&entry.target)),
                is_git_source: true,
                strategy: entry.strategy,
                encryption: entry.encryption.clone(),
                permissions: entry.permissions.clone(),
                patch: entry.patch.clone(),
            });
        } else if entry.source.is_empty() {
            // A `strategy: Patch` entry needs no source. Joining an empty
            // relative path onto the module directory would yield the module
            // directory itself, which every downstream `source.is_dir()` /
            // `source.exists()` branch would read as a deployable payload.
            resolved.push(ResolvedFile {
                source: PathBuf::new(),
                target: crate::expand_tilde(Path::new(&entry.target)),
                is_git_source: false,
                strategy: entry.strategy,
                encryption: entry.encryption.clone(),
                permissions: entry.permissions.clone(),
                patch: entry.patch.clone(),
            });
        } else {
            // Local path — relative to module directory
            let rel = std::path::Path::new(&entry.source);
            // `source: .` is the module's own directory — the documented way to
            // deploy a module's whole tree, and a legitimate answer here even
            // though it names nothing of its own.
            crate::validate_no_traversal_allowing_self(rel).map_err(|e| {
                ModuleError::InvalidSpec {
                    name: module.name.clone(),
                    message: format!("file source '{}' is not usable: {e}", entry.source),
                }
            })?;
            let source = module.dir.join(rel);
            // Verify the resolved path stays within the module directory
            // (prevents symlink-based escape from module boundary)
            if source.exists()
                && let (Ok(canonical_src), Ok(canonical_dir)) =
                    (source.canonicalize(), module.dir.canonicalize())
                && !canonical_src.starts_with(&canonical_dir)
            {
                return Err(ModuleError::InvalidSpec {
                    name: module.name.clone(),
                    message: format!(
                        "file source '{}' resolves outside module directory",
                        entry.source
                    ),
                }
                .into());
            }
            resolved.push(ResolvedFile {
                source,
                target: crate::expand_tilde(Path::new(&entry.target)),
                is_git_source: false,
                strategy: entry.strategy,
                encryption: entry.encryption.clone(),
                permissions: entry.permissions.clone(),
                patch: entry.patch.clone(),
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
    source_roots: &[SourceModuleRoot],
    platform: &Platform,
    managers: &HashMap<String, &dyn PackageManager>,
    printer: &crate::output::Printer,
) -> Result<Vec<ResolvedModule>> {
    let all_modules = load_all_modules(config_dir, cache_base, source_roots, printer)?;

    // Resolve profile references (e.g., "community/tmux" → "tmux") to actual module names
    let resolved_names: Vec<String> = requested
        .iter()
        .map(|r| resolve_profile_module_name(r).to_string())
        .collect();
    // tracing-ok: internal resolution-set diagnostic, not user-facing — nothing
    // else prints the requested module set before dependency order is walked
    tracing::debug!(names = ?resolved_names, "resolving modules");

    let order = resolve_dependency_order(&resolved_names, &all_modules)
        .map_err(|e| enrich_not_found(e, source_roots))?;

    // Determine platform-skipped modules up front so an active module that
    // depends on a skipped one can be rejected as a config error before any
    // package/file resolution runs.
    let skipped: HashSet<&str> = order
        .iter()
        .filter(|name| !platform.matches_any(&all_modules[*name].spec.platforms))
        .map(|name| name.as_str())
        .collect();

    validate_no_active_dependents_on_skipped(&order, &skipped, |name| {
        let spec = &all_modules[name].spec;
        (&spec.depends, &spec.platforms)
    })?;

    let mut resolved = Vec::new();
    // A module's own resolution is where this walk waits: a git file source
    // is cloned or fetched here and every manifest is read off disk. Narrated
    // per module at the one place every command's module walk goes through, so
    // the wait names what it is waiting on rather than standing silent.
    printer.narrate("Resolving modules", |sp| -> Result<()> {
        for name in &order {
            sp.set_message(format!("Resolving module:{name}"));
            let module = &all_modules[name];

            // Platform-gated out: emit a placeholder carrying the skip reason and
            // empty contents. The visible Skip action is produced by plan_modules.
            // Resolving packages/files here is wasteful and could error on the
            // other platform's assets, so it is deliberately skipped.
            if skipped.contains(name.as_str()) {
                resolved.push(ResolvedModule::skipped(
                    name.clone(),
                    module.dir.clone(),
                    module.spec.depends.clone(),
                    format!(
                        "platform not matched (requires: {})",
                        module.spec.platforms.join(", ")
                    ),
                    module.origin.clone(),
                ));
                continue;
            }

            let packages = resolve_module_packages(module, platform, managers)?;
            let files = resolve_module_files(module, cache_base, printer)?;

            let scripts = module.spec.scripts.as_ref();
            let pre_apply_scripts = scripts.map(|s| s.pre_apply.clone()).unwrap_or_default();
            let post_apply_scripts = scripts.map(|s| s.post_apply.clone()).unwrap_or_default();
            let pre_reconcile_scripts =
                scripts.map(|s| s.pre_reconcile.clone()).unwrap_or_default();
            let post_reconcile_scripts = scripts
                .map(|s| s.post_reconcile.clone())
                .unwrap_or_default();
            let on_change_scripts = scripts.map(|s| s.on_change.clone()).unwrap_or_default();
            let on_drift_scripts = scripts.map(|s| s.on_drift.clone()).unwrap_or_default();

            resolved.push(ResolvedModule {
                name: name.clone(),
                packages,
                files,
                env: module.spec.env.clone(),
                aliases: module.spec.aliases.clone(),
                system: module.spec.system.clone(),
                pre_apply_scripts,
                post_apply_scripts,
                pre_reconcile_scripts,
                post_reconcile_scripts,
                on_change_scripts,
                on_drift_scripts,
                depends: module.spec.depends.clone(),
                dir: module.dir.clone(),
                platform_skip_reason: None,
                origin: module.origin.clone(),
            });
        }
        Ok(())
    })?;

    Ok(resolved)
}

/// Enrich a `ModuleError::NotFound` raised during dependency resolution: when the
/// missing name appears in some source root's `offered` allow-list, the publisher
/// declared it in `provides.modules` but failed to deliver the body — surface that
/// as `OfferedButMissing` naming the source. When several roots offer the name, the
/// highest-priority one is named (tie-break on source_name) so the message matches
/// the source that would have won the body race. All other errors pass through;
/// both variants keep the exit-6 NotFound code.
fn enrich_not_found(err: CfgdError, source_roots: &[SourceModuleRoot]) -> CfgdError {
    if let CfgdError::Module(ModuleError::NotFound { name }) = &err
        && let Some(root) = source_roots
            .iter()
            .filter(|root| root.offered.iter().any(|m| m == name))
            .max_by(|a, b| {
                a.priority
                    .cmp(&b.priority)
                    .then_with(|| b.source_name.cmp(&a.source_name))
            })
    {
        return ModuleError::OfferedButMissing {
            name: name.clone(),
            source_name: root.source_name.clone(),
        }
        .into();
    }
    err
}

/// Reject an active module that depends on a platform-skipped one.
///
/// Pure (no I/O): `order` is the resolution order, `skipped` the set of
/// lookup-names gated out on the current platform, and `lookup` returns each
/// module's `(depends, platforms)`. A skipped module's own dependencies are not
/// checked — it will never run. Dependency names pass through
/// `resolve_profile_module_name` before comparison for robustness, though the
/// loader has already required each `depends` entry to be a bare module key by
/// the time this runs.
pub(crate) fn validate_no_active_dependents_on_skipped<'a, F>(
    order: &'a [String],
    skipped: &HashSet<&str>,
    lookup: F,
) -> Result<()>
where
    F: Fn(&'a str) -> (&'a [String], &'a [String]),
{
    for name in order {
        if skipped.contains(name.as_str()) {
            continue;
        }
        let (depends, _) = lookup(name);
        for dep in depends {
            let dep_name = resolve_profile_module_name(dep);
            if skipped.contains(dep_name) {
                let (_, dep_platforms) = lookup(dep_name);
                return Err(ModuleError::DependencyPlatformSkipped {
                    module: name.clone(),
                    dependency: dep_name.to_string(),
                    platforms: dep_platforms.join(", "),
                }
                .into());
            }
        }
    }
    Ok(())
}
