//! Package manager implementations and the package reconciler.
//!
//! Each package manager (`brew`, `cargo`, `npm`, ...) lives in its own
//! submodule. Shared helpers (process execution, sudo, brew detection,
//! caveat extraction) live in `shared`. Pure helpers (`parsers`,
//! `versions`) sit beside `simple`, which uses them.
//!
//! `mod.rs` itself owns:
//! - The `pub use` re-exports so callers continue to address types as
//!   `crate::packages::BrewManager`, `crate::packages::plan_packages`, etc.
//! - The reconciler (`plan_packages`, `apply_packages`, ...).
//! - `add_package` / `remove_package` profile-spec mutators.
//! - Native-manifest parsers (Brewfile, package.json, Cargo.toml, apt list)
//!   and `resolve_manifest_packages`.
//! - The provider registry (`all_package_managers`).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use cfgd_core::PathDisplayExt;
use cfgd_core::config::{LOCAL_LAYER, MergedProfile, PackagesSpec};
use cfgd_core::effective::effective_desired_packages;
use cfgd_core::errors::{PackageError, Result};
use cfgd_core::modules::ResolvedModule;
use cfgd_core::output::Role;
use cfgd_core::providers::{
    OrphanedPackage, PackageAction, PackageContext, PackageManager, PackageManagerExt,
};
use cfgd_core::reconciler::ActualPackages;

mod brew;
mod cargo;
mod choco;
mod flatpak;
mod go;
mod nix;
mod npm;
mod parsers;
mod pipx;
mod scoop;
mod scripted;
mod shared;
mod simple;
mod snap;
mod versions;
mod winget;

pub use brew::{BrewCaskManager, BrewManager, BrewTapManager};
pub use cargo::CargoManager;
pub use choco::ChocolateyManager;
pub use flatpak::FlatpakManager;
pub use go::GoInstallManager;
pub use nix::NixManager;
pub use npm::NpmManager;
pub use pipx::PipxManager;
pub use scoop::ScoopManager;
pub use scripted::custom_managers;
// SimpleManager and ScriptedManager are part of the package-manager type
// surface; they aren't named directly by callers today (constructed via
// `all_package_managers()` / `custom_managers()`), but the re-exports keep
// the types reachable at `crate::packages::SimpleManager` / `ScriptedManager`.
#[allow(unused_imports)]
pub use scripted::ScriptedManager;
#[allow(unused_imports)]
pub use simple::SimpleManager;
pub use snap::SnapManager;
pub use winget::WingetManager;

use simple::{
    apk_manager, apt_manager, dnf_manager, pacman_manager, pkg_manager, yum_manager, zypper_manager,
};

// --- Package Reconciler ---

/// Compute the packages to prune for one manager: cfgd-tracked, still installed,
/// no longer desired. User-installed packages (not in `cfgd_installed`) are
/// never returned, so they survive a prune even when installed-and-not-desired.
///
/// `installed` holds identity names (what `installed_packages` reports), and the
/// tracking key is `<manager>/<identity>`. Desired entries are mapped through
/// `package_identity` so a manager whose install argument differs from its
/// listed name (e.g. go: `rsc.io/2fa` → `2fa`) compares like with like; the
/// returned values are identities, which is exactly what `uninstall` expects.
fn uninstall_for_manager(
    manager: &dyn PackageManager,
    desired: &[String],
    installed: &HashSet<String>,
    cfgd_installed: &HashSet<String>,
) -> Vec<String> {
    let desired_identities: HashSet<String> = desired
        .iter()
        .map(|p| manager.package_identity(p))
        .collect();
    let name = manager.name();
    installed
        .iter()
        .filter(|pkg| {
            !desired_identities.contains(*pkg) && cfgd_installed.contains(&format!("{name}/{pkg}"))
        })
        .cloned()
        .collect()
}

/// Plan package actions by diffing installed vs desired for all managers.
/// An unavailable manager that can be bootstrapped still gets its Install
/// action planned here; provisioning the manager itself is the Prerequisites
/// phase's job (`ManagerAction::Provision`), planned separately.
///
/// `cfgd_installed` carries the set of packages cfgd itself installed, as
/// `"<manager>/<identity>"` entries (the installed-DB identity name — i.e. what
/// `installed_packages` reports, which for go is the binary name). It bounds
/// declarative prune: a package is only ever uninstalled when cfgd installed it,
/// it is still on the system, and it has left the desired set — so packages the
/// user installed outside cfgd are never removed.
pub fn plan_packages(
    profile: &MergedProfile,
    modules: &[ResolvedModule],
    managers: &[&dyn PackageManager],
    cfgd_installed: &HashSet<String>,
    cx: &PackageContext<'_>,
) -> Result<Vec<PackageAction>> {
    Ok(plan_packages_observed(profile, modules, managers, cfgd_installed, cx)?.0)
}

/// The observation's version for one listed package: the version the manager
/// reported, unless it reported none. `"unknown"` is the
/// [`PackageManager::installed_packages_with_versions`] contract's sentinel
/// for "this manager does not know", and an empty string is the same answer.
fn known_version(pkg: &cfgd_core::providers::PackageInfo) -> Option<String> {
    let v = pkg.version.trim();
    if v.is_empty() || v == "unknown" {
        None
    } else {
        Some(v.to_string())
    }
}

/// [`plan_packages`] plus what its enumeration observed, for the callers that
/// also classify source decisions.
///
/// The captured [`ActualPackages`] is the planner's OWN installed-state read —
/// the single `installed_packages_with_versions` call and the same
/// `package_identity` mapping the diff below runs on — so the source-decision
/// auto-accept judges presence (and version satisfaction) exactly as the plan
/// does, with no second shell-out. Only available managers are recorded: an
/// unavailable or erroring manager contributes nothing, and the
/// classification fails closed for its packages.
pub fn plan_packages_observed(
    profile: &MergedProfile,
    modules: &[ResolvedModule],
    managers: &[&dyn PackageManager],
    cfgd_installed: &HashSet<String>,
    cx: &PackageContext<'_>,
) -> Result<(Vec<PackageAction>, ActualPackages)> {
    let mut actions = Vec::new();
    let mut actual = ActualPackages::default();

    // Single-source the desired set from the effective (profile ⊕ modules) view
    // so this planner sees exactly what every other read/write surface does.
    // With `modules` empty this equals the profile's own desired packages, so
    // the profile-scoped write path is unchanged.
    let effective = effective_desired_packages(profile, modules);
    // Grouped once, ahead of both passes: each pass asks every manager what it
    // wants, and a per-manager filter over the whole effective list walks it
    // twice per manager — quadratic in a config whose modules declare a few
    // hundred packages across a dozen managers.
    let mut desired_by_manager: HashMap<&str, Vec<String>> = HashMap::new();
    for pkg in &effective {
        desired_by_manager
            .entry(pkg.manager.as_str())
            .or_default()
            .push(pkg.name.clone());
    }
    let desired_for = |manager_name: &str| -> Vec<String> {
        desired_by_manager
            .get(manager_name)
            .cloned()
            .unwrap_or_default()
    };

    // Asked once per manager, ahead of both passes: `is_available()` is a PATH
    // probe (and for some managers a shell-out), and the two passes below ask
    // the same managers the same question with nothing between them that could
    // change the answer.
    let availability: Vec<bool> = managers.iter().map(|m| m.is_available()).collect();

    // Pass 1: determine which managers will be bootstrapped
    let mut bootstrapping: HashSet<String> = HashSet::new();
    for (manager, available) in managers.iter().zip(&availability) {
        if !desired_by_manager.contains_key(manager.name()) {
            continue;
        }
        if !available && manager.can_bootstrap() {
            bootstrapping.insert(manager.name().to_string());
        }
    }

    // Pass 2: generate actions. A source-registering manager (`brew-tap`)
    // goes ahead of its OWN family — its entries are the repositories the
    // family's other installs may resolve from — and no further: a tap has
    // nothing to say about another family's packages, so hoisting it globally
    // would reorder managers it cannot affect. Families keep the order of
    // their first registry appearance, so the list stays deterministic.
    let mut family_rank: HashMap<&str, usize> = HashMap::new();
    for (i, manager) in managers.iter().enumerate() {
        family_rank
            .entry(cfgd_core::manager_family(manager.name()))
            .or_insert(i);
    }
    let mut order: Vec<usize> = (0..managers.len()).collect();
    order.sort_by_key(|&i| {
        (
            // Populated from this same slice one loop up, so every family is
            // present — but a sort key must not be able to panic mid-plan, so
            // an absent family sorts last instead of unwinding.
            family_rank
                .get(cfgd_core::manager_family(managers[i].name()))
                .copied()
                .unwrap_or(usize::MAX),
            !managers[i].registers_family_sources(),
        )
    });
    for (manager, available) in order.iter().map(|&i| (&managers[i], &availability[i])) {
        let desired = desired_for(manager.name());

        // A manager with no desired packages AND no cfgd-tracked installs has
        // nothing to do — skip it without touching the system. Reading
        // installed state for an idle manager is both wasteful and unsafe: a
        // partially-installed manager (e.g. pacman present but its db
        // unreadable) would error out and abort the whole plan even though no
        // package under it is in play. Prune still fires when the LAST package
        // is dropped because the dropped package leaves a tracked entry behind,
        // so `has_tracked` stays true even as `desired` empties.
        let mgr_prefix = format!("{}/", manager.name());
        let has_tracked = cfgd_installed.iter().any(|id| id.starts_with(&mgr_prefix));
        if desired.is_empty() && !has_tracked {
            continue;
        }

        // Prune is computed independently of `desired` so dropping the LAST
        // package from a manager still removes its cfgd-tracked installs.
        // Only available managers can read installed state to confirm the
        // package is still present before pruning.
        if *available {
            // ONE enumeration serves both the install/prune diff and the
            // source-decision observation, and the context's memo is what
            // makes it one for the whole command: the version half the
            // satisfies-gate judges a pinned source item against comes from
            // the same read the diff below compares against. Listed names fold
            // through `listed_identity` — NOT `package_identity`, which maps
            // declared entries and need not be a fixed point over listed names
            // — so the diff still compares the exact identity space it always
            // has (a case-insensitive manager's display-case listing folds to
            // its lowercase identity form; everyone else's listing already
            // reports identities and passes through untouched). Managers whose
            // enumeration reports no version record `None`, and a pinned item
            // under them stays pending (fail-closed).
            let enumerated = cx.installed_for(*manager)?;
            let installed = enumerated.identities();
            actual.record_enumeration(
                manager.name(),
                enumerated
                    .listed()
                    .iter()
                    .map(|pkg| (manager.listed_identity(&pkg.name), known_version(pkg))),
            );
            for entry in &desired {
                actual.record_identity(manager.name(), entry, &manager.package_identity(entry));
                // A version-pinned entry is judged by its BARE name; record
                // that name's identity too, so the classification looks the
                // pin up in the same folded space the listing above uses.
                if let Some((bare, _)) = entry.rsplit_once('@')
                    && !bare.is_empty()
                {
                    actual.record_identity(manager.name(), bare, &manager.package_identity(bare));
                }
            }

            // Install before uninstall so a rename (old pkg dropped, new pkg
            // added) lands the replacement before removing the old. The diff
            // compares by IDENTITY (what `installed_packages` reports), not the
            // raw entry: for go, `rsc.io/2fa` installs as binary `2fa`, so a
            // raw-string compare would always re-install. The Install action
            // still carries the ORIGINAL entries so `go install` gets the full
            // module path.
            let to_install: Vec<String> = desired
                .iter()
                .filter(|p| !installed.contains(&manager.package_identity(p)))
                .cloned()
                .collect();
            if !to_install.is_empty() {
                actions.push(PackageAction::Install {
                    manager: manager.name().to_string(),
                    packages: to_install,
                    origin: LOCAL_LAYER.to_string(),
                });
            }

            let to_uninstall = uninstall_for_manager(*manager, &desired, installed, cfgd_installed);
            if !to_uninstall.is_empty() {
                actions.push(PackageAction::Uninstall {
                    manager: manager.name().to_string(),
                    packages: to_uninstall,
                    origin: LOCAL_LAYER.to_string(),
                });
            }
        } else if desired.is_empty() {
            // Unavailable manager with only tracked installs and nothing desired:
            // it cannot read installed state to confirm presence, so it cannot
            // safely prune — leave its packages untouched.
            continue;
        } else if manager.can_bootstrap() {
            // Unavailable but bootstrappable: the Prerequisites phase plans
            // provisioning this manager separately (`ManagerAction::Provision`).
            // Install all desired packages so they land once it lands.
            actions.push(PackageAction::Install {
                manager: manager.name().to_string(),
                packages: desired,
                origin: LOCAL_LAYER.to_string(),
            });
        } else if bootstrapping.contains(cfgd_core::manager_family(manager.name())) {
            // Sub-manager whose parent is being bootstrapped (e.g. brew-tap when brew
            // is being bootstrapped). Install all desired — nothing is installed yet.
            actions.push(PackageAction::Install {
                manager: manager.name().to_string(),
                packages: desired,
                origin: LOCAL_LAYER.to_string(),
            });
        } else {
            actions.push(PackageAction::Skip {
                manager: manager.name().to_string(),
                reason: format!(
                    "'{}' not available — cannot auto-install on this platform",
                    manager.name()
                ),
                origin: LOCAL_LAYER.to_string(),
            });
        }
    }

    Ok((actions, actual))
}

/// Apply package actions.
#[cfg(test)]
pub fn apply_packages(
    actions: &[PackageAction],
    managers: &[&dyn PackageManager],
    cx: &PackageContext<'_>,
) -> Result<()> {
    for action in actions {
        match action {
            PackageAction::Install {
                manager: mgr_name,
                packages,
                ..
            } => {
                if let Some(mgr) = managers.iter().find(|m| m.name() == mgr_name) {
                    mgr.install(packages, cx)?;
                }
            }
            PackageAction::Uninstall {
                manager: mgr_name,
                packages,
                ..
            } => {
                if let Some(mgr) = managers.iter().find(|m| m.name() == mgr_name) {
                    mgr.uninstall(packages, cx)?;
                }
            }
            PackageAction::Skip {
                manager, reason, ..
            } => {
                cx.report(Role::Warn, manager, reason);
            }
        }
    }

    Ok(())
}

/// Add a package to the profile's package spec.
pub fn add_package(
    manager_name: &str,
    package_name: &str,
    packages: &mut PackagesSpec,
) -> Result<()> {
    match manager_name {
        "brew" => {
            let brew = packages.brew.get_or_insert_with(Default::default);
            if !brew.formulae.contains(&package_name.to_string()) {
                brew.formulae.push(package_name.to_string());
            }
        }
        "brew-tap" => {
            let brew = packages.brew.get_or_insert_with(Default::default);
            if !brew.taps.contains(&package_name.to_string()) {
                brew.taps.push(package_name.to_string());
            }
        }
        "brew-cask" => {
            let brew = packages.brew.get_or_insert_with(Default::default);
            if !brew.casks.contains(&package_name.to_string()) {
                brew.casks.push(package_name.to_string());
            }
        }
        "apt" => {
            let apt = packages.apt.get_or_insert_with(Default::default);
            if !apt.packages.contains(&package_name.to_string()) {
                apt.packages.push(package_name.to_string());
            }
        }
        "cargo" => {
            let cargo = packages.cargo.get_or_insert_with(Default::default);
            if !cargo.packages.contains(&package_name.to_string()) {
                cargo.packages.push(package_name.to_string());
            }
        }
        "npm" => {
            let npm = packages.npm.get_or_insert_with(Default::default);
            if !npm.global.contains(&package_name.to_string()) {
                npm.global.push(package_name.to_string());
            }
        }
        "snap" => {
            let snap = packages.snap.get_or_insert_with(Default::default);
            if !snap.packages.contains(&package_name.to_string()) {
                snap.packages.push(package_name.to_string());
            }
        }
        // Not a registered manager: `snap` installs both lists and retries with
        // `--classic`. The key exists so `--package snap.classic:code` reaches
        // the sub-list the schema splits out, and it is never persisted.
        "snap-classic" => {
            let snap = packages.snap.get_or_insert_with(Default::default);
            if !snap.classic.contains(&package_name.to_string()) {
                snap.classic.push(package_name.to_string());
            }
        }
        "flatpak" => {
            let flatpak = packages.flatpak.get_or_insert_with(Default::default);
            if !flatpak.packages.contains(&package_name.to_string()) {
                flatpak.packages.push(package_name.to_string());
            }
        }
        _ => {
            // Simple Vec<String> managers (pipx, dnf, apk, pacman, zypper, yum, pkg, nix, go,
            // winget, chocolatey, scoop) delegate through simple_list_mut.
            if let Some(list) = packages.simple_list_mut(manager_name) {
                if !list.contains(&package_name.to_string()) {
                    list.push(package_name.to_string());
                }
            } else if let Some(custom) = packages.custom.iter_mut().find(|c| c.name == manager_name)
            {
                if !custom.packages.contains(&package_name.to_string()) {
                    custom.packages.push(package_name.to_string());
                }
            } else {
                // `manager_name` matches none of the known spec slots and no
                // declared `custom` entry — this schema has no runtime
                // registry to consult, so reaching here always means the
                // name was never registered, never merely unprovisioned.
                return Err(PackageError::ManagerNotFound {
                    manager: manager_name.to_string(),
                }
                .into());
            }
        }
    }
    Ok(())
}

/// Remove a package from the profile's package spec.
pub fn remove_package(
    manager_name: &str,
    package_name: &str,
    packages: &mut PackagesSpec,
) -> Result<bool> {
    let removed = match manager_name {
        "brew" => {
            if let Some(ref mut brew) = packages.brew {
                let before = brew.formulae.len();
                brew.formulae.retain(|p| p != package_name);
                brew.formulae.len() < before
            } else {
                false
            }
        }
        "brew-tap" => {
            if let Some(ref mut brew) = packages.brew {
                let before = brew.taps.len();
                brew.taps.retain(|p| p != package_name);
                brew.taps.len() < before
            } else {
                false
            }
        }
        "brew-cask" => {
            if let Some(ref mut brew) = packages.brew {
                let before = brew.casks.len();
                brew.casks.retain(|p| p != package_name);
                brew.casks.len() < before
            } else {
                false
            }
        }
        "snap-classic" => {
            if let Some(ref mut snap) = packages.snap {
                let before = snap.classic.len();
                snap.classic.retain(|p| p != package_name);
                snap.classic.len() < before
            } else {
                false
            }
        }
        "apt" => {
            if let Some(ref mut apt) = packages.apt {
                let before = apt.packages.len();
                apt.packages.retain(|p| p != package_name);
                apt.packages.len() < before
            } else {
                false
            }
        }
        "cargo" => {
            if let Some(ref mut cargo) = packages.cargo {
                let before = cargo.packages.len();
                cargo.packages.retain(|p| p != package_name);
                cargo.packages.len() < before
            } else {
                false
            }
        }
        "npm" => {
            if let Some(ref mut npm) = packages.npm {
                let before = npm.global.len();
                npm.global.retain(|p| p != package_name);
                npm.global.len() < before
            } else {
                false
            }
        }
        "snap" => {
            if let Some(ref mut snap) = packages.snap {
                let before = snap.packages.len() + snap.classic.len();
                snap.packages.retain(|p| p != package_name);
                snap.classic.retain(|p| p != package_name);
                (snap.packages.len() + snap.classic.len()) < before
            } else {
                false
            }
        }
        "flatpak" => {
            if let Some(ref mut flatpak) = packages.flatpak {
                let before = flatpak.packages.len();
                flatpak.packages.retain(|p| p != package_name);
                flatpak.packages.len() < before
            } else {
                false
            }
        }
        _ => {
            // Simple Vec<String> managers (pipx, dnf, apk, pacman, zypper, yum, pkg, nix, go,
            // winget, chocolatey, scoop) delegate through simple_list_mut.
            if let Some(list) = packages.simple_list_mut(manager_name) {
                let before = list.len();
                list.retain(|p| p != package_name);
                list.len() < before
            } else if let Some(custom) = packages.custom.iter_mut().find(|c| c.name == manager_name)
            {
                let before = custom.packages.len();
                custom.packages.retain(|p| p != package_name);
                custom.packages.len() < before
            } else {
                // Same reasoning as `add_package`'s fallback: no declared
                // slot for this name means it was never registered.
                return Err(PackageError::ManagerNotFound {
                    manager: manager_name.to_string(),
                }
                .into());
            }
        }
    };
    Ok(removed)
}

/// How a `SimpleManager` family installs a package list, for a surface that
/// EMITS the commands rather than running them.
///
/// `cfgd module export` wrote its own `apt-get install -y --no-install-recommends`
/// while the apply path ran the family's `install_cmd` (`apt-get install -y`), so
/// one `module.yaml` resolved two different package sets depending on which of
/// cfgd's own commands you asked. The family owns HOW it installs; a surface that
/// emits an install composes from here, and a hand-written install verb in a
/// `format!` is the bug this exists to make unwritable.
///
/// `sudo` is stripped unconditionally (`SimpleManager::export_cmd`): the
/// consumers are container build scripts, which already run as root, so the
/// privilege of the host composing the script says nothing about it.
pub struct ManagerInstallScript {
    /// The index refresh the family declares, if it has one.
    pub update: Option<String>,
    /// The install itself, with `packages` appended.
    pub install: String,
}

/// [`ManagerInstallScript`] for a family name, or `None` for a manager that is
/// not one of the data-driven system families.
pub fn manager_install_script(manager: &str, packages: &[String]) -> Option<ManagerInstallScript> {
    let mgr = simple::simple_manager(manager)?;
    Some(ManagerInstallScript {
        update: mgr.update_cmd.map(|cmd| mgr.export_cmd(cmd, &[])),
        install: mgr.export_cmd(mgr.install_cmd, packages),
    })
}

/// Build the default provider registry with all workstation package managers.
pub fn all_package_managers() -> Vec<Box<dyn PackageManager>> {
    vec![
        Box::new(BrewManager),
        Box::new(BrewTapManager),
        Box::new(BrewCaskManager),
        Box::new(apt_manager()),
        Box::new(CargoManager),
        Box::new(NpmManager),
        Box::new(PipxManager),
        Box::new(dnf_manager()),
        Box::new(apk_manager()),
        Box::new(pacman_manager()),
        Box::new(zypper_manager()),
        Box::new(yum_manager()),
        Box::new(pkg_manager()),
        Box::new(SnapManager),
        Box::new(FlatpakManager),
        Box::new(NixManager),
        Box::new(GoInstallManager),
        Box::new(WingetManager),
        Box::new(ChocolateyManager),
        Box::new(ScoopManager),
    ]
}

/// Run the persisted uninstall command for each orphaned custom-manager package
/// (its manager block left the config). Returns the `(manager, package)` rows
/// that were uninstalled successfully, for the caller to GC. Groups by manager so
/// a batch template runs once; a failed uninstall leaves its row intact (warned)
/// so a later run can retry. Rows with no persisted command are reported via the
/// printer and skipped (cannot remove what we have no script for).
pub fn prune_orphaned_packages(
    orphans: &[OrphanedPackage],
    cx: &PackageContext<'_>,
) -> Vec<(String, String)> {
    // Group by (manager, uninstall_cmd) so each scripted manager's persisted
    // template runs once over its full package batch.
    let mut groups: std::collections::BTreeMap<(String, String), Vec<String>> =
        std::collections::BTreeMap::new();
    let mut removed = Vec::new();

    for orphan in orphans {
        match &orphan.uninstall_cmd {
            Some(cmd) => groups
                .entry((orphan.manager.clone(), cmd.clone()))
                .or_default()
                .push(orphan.package.clone()),
            None => {
                cx.report(
                    Role::Warn,
                    &orphan.manager,
                    format!(
                        "orphaned {}/{} tracked but its custom manager left the config with no persisted uninstall script — remove it manually",
                        orphan.manager, orphan.package
                    ),
                );
            }
        }
    }

    for ((manager, uninstall_cmd), packages) in groups {
        let mgr = ScriptedManager::from_uninstall_only(&manager, uninstall_cmd);
        let outcome = mgr.uninstall(&packages, cx);
        // A persisted uninstall script takes binaries off the machine exactly
        // as a manager's own uninstall does, and a partial failure has already
        // removed whatever it removed — so the memos describing what resolves
        // and what is installed are retired either way.
        cfgd_core::invalidate_command_resolution();
        match outcome {
            Ok(()) => {
                for pkg in packages {
                    removed.push((manager.clone(), pkg));
                }
            }
            Err(e) => {
                cx.report(
                    Role::Warn,
                    &manager,
                    format!(
                        "failed to uninstall orphaned packages via {manager}: {}",
                        cfgd_core::output::collapse_to_subject_line(&e)
                    ),
                );
            }
        }
    }

    removed
}

// --- Native manifest support ---

/// Parse a Brewfile and extract taps, formulae, and casks.
/// Brewfile format: lines like `tap "name"`, `brew "name"`, `cask "name"`.
/// Comments (#) and blank lines are ignored.
fn parse_brewfile(path: &Path) -> Result<(Vec<String>, Vec<String>, Vec<String>)> {
    let content = std::fs::read_to_string(path).map_err(|e| PackageError::ListFailed {
        manager: "brew".into(),
        message: format!("failed to read Brewfile {}: {}", path.posix(), e),
    })?;

    let mut taps = Vec::new();
    let mut formulae = Vec::new();
    let mut casks = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Extract the quoted name from lines like: brew "ripgrep", tap "homebrew/cask"
        // Also handle comma-separated options after the name
        if let Some(name) = extract_brewfile_name(line) {
            if line.starts_with("tap ") {
                taps.push(name);
            } else if line.starts_with("brew ") {
                formulae.push(name);
            } else if line.starts_with("cask ") {
                casks.push(name);
            }
            // Ignore mas, vscode, whalebrew, etc.
        }
    }

    Ok((taps, formulae, casks))
}

/// Extract the package name from a Brewfile line.
/// Handles: `brew "name"`, `brew "name", args: ...`, `brew 'name'`
fn extract_brewfile_name(line: &str) -> Option<String> {
    // Find the first quoted string after the keyword
    let after_keyword = line.split_once(' ')?.1.trim();
    if let Some(rest) = after_keyword.strip_prefix('"') {
        rest.split('"').next().map(|s| s.to_string())
    } else if let Some(rest) = after_keyword.strip_prefix('\'') {
        rest.split('\'').next().map(|s| s.to_string())
    } else {
        // Unquoted: take until comma or end of line
        Some(
            after_keyword
                .split(',')
                .next()
                .unwrap_or(after_keyword)
                .trim()
                .to_string(),
        )
    }
}

/// Parse an apt package list file (one package per line).
/// Comments (#) and blank lines are ignored.
fn parse_apt_manifest(path: &Path) -> Result<Vec<String>> {
    let content = std::fs::read_to_string(path).map_err(|e| PackageError::ListFailed {
        manager: "apt".into(),
        message: format!("failed to read apt manifest {}: {}", path.posix(), e),
    })?;

    Ok(content
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.to_string())
        .collect())
}

/// Parse a package.json and extract dependency names.
/// Reads `dependencies` and `devDependencies` keys.
fn parse_npm_package_json(path: &Path) -> Result<Vec<String>> {
    let content = std::fs::read_to_string(path).map_err(|e| PackageError::ListFailed {
        manager: "npm".into(),
        message: format!("failed to read package.json {}: {}", path.posix(), e),
    })?;

    let json: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| PackageError::ListFailed {
            manager: "npm".into(),
            message: format!("failed to parse package.json {}: {}", path.posix(), e),
        })?;

    let mut packages = Vec::new();

    for section in ["dependencies", "devDependencies"] {
        if let Some(deps) = json.get(section).and_then(|v| v.as_object()) {
            for key in deps.keys() {
                if !packages.contains(key) {
                    packages.push(key.clone());
                }
            }
        }
    }

    Ok(packages)
}

/// Parse a Cargo.toml and extract dependency names.
/// Reads the `[dependencies]` table keys.
fn parse_cargo_toml(path: &Path) -> Result<Vec<String>> {
    let content = std::fs::read_to_string(path).map_err(|e| PackageError::ListFailed {
        manager: "cargo".into(),
        message: format!("failed to read Cargo.toml {}: {}", path.posix(), e),
    })?;

    let toml_val: toml::Value = toml::from_str(&content).map_err(|e| PackageError::ListFailed {
        manager: "cargo".into(),
        message: format!("failed to parse Cargo.toml {}: {}", path.posix(), e),
    })?;

    let mut packages = Vec::new();

    if let Some(deps) = toml_val.get("dependencies").and_then(|v| v.as_table()) {
        for key in deps.keys() {
            packages.push(key.clone());
        }
    }

    Ok(packages)
}

/// What one manifest file parsed to, as stored in a [`ManifestCache`].
#[derive(Clone)]
enum ParsedManifest {
    /// A Brewfile's `(taps, formulae, casks)`.
    Brew(Vec<String>, Vec<String>, Vec<String>),
    /// Every other manifest shape: a flat list of package names.
    Names(Vec<String>),
}

/// The identity of the bytes a cached parse describes: modification time and
/// length. A manifest a lifecycle hook rewrote mid-run changes at least one of
/// them, so the next lookup re-reads instead of merging what the file used to
/// say.
type ManifestStamp = (Option<std::time::SystemTime>, u64);

/// One cached manifest parse: the identity of the bytes it describes, and what
/// they parsed to.
type CachedManifest = (ManifestStamp, ParsedManifest);

/// The manifest files already parsed during one run, keyed by path and kind.
///
/// [`resolve_manifest_packages`] runs twice on a `status` / `diff` / `plan`
/// invocation — once over the composed profile and once over the local-only
/// profile the source-decision scope is classified against — and both passes
/// read and parse the SAME Brewfile, `package.json` and `Cargo.toml` off disk.
/// The cache is keyed on the FILE rather than on the spec being filled, so the
/// second pass is one `metadata()` call whichever spec it is merging into.
///
/// Deliberately not process-global: a manifest is only stable for the length of
/// one run, and a cache outliving the run would hand a daemon tick the contents
/// the file had an interval ago.
#[derive(Default)]
pub struct ManifestCache {
    entries: std::cell::RefCell<HashMap<(PathBuf, &'static str), CachedManifest>>,
}

impl ManifestCache {
    fn stamp(path: &Path) -> ManifestStamp {
        match std::fs::metadata(path) {
            Ok(meta) => (meta.modified().ok(), meta.len()),
            Err(_) => (None, 0),
        }
    }

    /// The parse of `path` under `kind`, reusing this run's result when the file
    /// has not changed since it was read.
    fn get_or_parse(
        &self,
        path: &Path,
        kind: &'static str,
        parse: impl FnOnce(&Path) -> Result<ParsedManifest>,
    ) -> Result<ParsedManifest> {
        let stamp = Self::stamp(path);
        let key = (path.to_path_buf(), kind);
        if let Some((cached_stamp, parsed)) = self.entries.borrow().get(&key)
            && *cached_stamp == stamp
        {
            return Ok(parsed.clone());
        }
        let parsed = parse(path)?;
        self.entries
            .borrow_mut()
            .insert(key, (stamp, parsed.clone()));
        Ok(parsed)
    }

    fn names(
        &self,
        path: &Path,
        kind: &'static str,
        parse: fn(&Path) -> Result<Vec<String>>,
    ) -> Result<Vec<String>> {
        match self.get_or_parse(path, kind, |p| parse(p).map(ParsedManifest::Names))? {
            ParsedManifest::Names(names) => Ok(names),
            ParsedManifest::Brew(..) => Ok(Vec::new()),
        }
    }
}

/// Resolve manifest files referenced in package specs and merge their contents
/// into the inline package lists. Paths are relative to `config_dir`.
///
/// Every parse is fresh. A caller that resolves manifests more than once in a
/// run reaches [`resolve_manifest_packages_cached`] with the run's
/// [`ManifestCache`] instead.
pub fn resolve_manifest_packages(packages: &mut PackagesSpec, config_dir: &Path) -> Result<()> {
    resolve_manifest_packages_cached(packages, config_dir, &ManifestCache::default())
}

/// [`resolve_manifest_packages`], reading each manifest at most once per run.
pub fn resolve_manifest_packages_cached(
    packages: &mut PackagesSpec,
    config_dir: &Path,
    cache: &ManifestCache,
) -> Result<()> {
    // Brew: parse Brewfile, merge taps/formulae/casks
    if let Some(ref mut brew) = packages.brew
        && let Some(ref file) = brew.file
    {
        let path = config_dir.join(file);
        if path.exists()
            && let ParsedManifest::Brew(taps, formulae, casks) =
                cache.get_or_parse(&path, "brew", |p| {
                    parse_brewfile(p).map(|(t, f, c)| ParsedManifest::Brew(t, f, c))
                })?
        {
            cfgd_core::union_extend(&mut brew.taps, &taps);
            cfgd_core::union_extend(&mut brew.formulae, &formulae);
            cfgd_core::union_extend(&mut brew.casks, &casks);
        }
    }

    // Apt: parse one-per-line file
    if let Some(ref mut apt) = packages.apt
        && let Some(ref file) = apt.file
    {
        let path = config_dir.join(file);
        if path.exists() {
            let pkgs = cache.names(&path, "apt", parse_apt_manifest)?;
            cfgd_core::union_extend(&mut apt.packages, &pkgs);
        }
    }

    // Npm: parse package.json
    if let Some(ref mut npm) = packages.npm
        && let Some(ref file) = npm.file
    {
        let path = config_dir.join(file);
        if path.exists() {
            let pkgs = cache.names(&path, "npm", parse_npm_package_json)?;
            cfgd_core::union_extend(&mut npm.global, &pkgs);
        }
    }

    // Cargo: parse Cargo.toml
    if let Some(ref mut cargo) = packages.cargo
        && let Some(ref file) = cargo.file
    {
        let path = config_dir.join(file);
        if path.exists() {
            let pkgs = cache.names(&path, "cargo", parse_cargo_toml)?;
            cfgd_core::union_extend(&mut cargo.packages, &pkgs);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests;
