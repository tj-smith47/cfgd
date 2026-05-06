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
//! - `bootstrap_method` cascade detection.
//! - Native-manifest parsers (Brewfile, package.json, Cargo.toml, apt list)
//!   and `resolve_manifest_packages`.
//! - The provider registry (`all_package_managers`).

use std::collections::HashSet;
use std::path::Path;

use cfgd_core::command_available;
use cfgd_core::config::{MergedProfile, PackagesSpec};
use cfgd_core::errors::{PackageError, Result};
#[cfg(test)]
use cfgd_core::output::Printer;
use cfgd_core::providers::{PackageAction, PackageManager};

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

use shared::brew_available;
use simple::{
    apk_manager, apt_manager, dnf_manager, pacman_manager, pkg_manager, yum_manager, zypper_manager,
};

// --- Package Reconciler ---

/// Bootstrap method description for display in plan/doctor output.
/// Detect which method will be used to bootstrap via brew→apt→dnf cascade.
fn detect_brew_system_method(fallback: &'static str) -> &'static str {
    if brew_available() {
        "brew"
    } else if command_available("apt") {
        "apt"
    } else if command_available("dnf") {
        "dnf"
    } else {
        fallback
    }
}

/// Detect which method will be used to bootstrap via apt→dnf→zypper cascade.
fn detect_system_method() -> &'static str {
    if command_available("apt") {
        "apt"
    } else if command_available("dnf") {
        "dnf"
    } else {
        "zypper"
    }
}

pub fn bootstrap_method(manager: &dyn PackageManager) -> &'static str {
    match manager.name() {
        "brew" => "homebrew installer",
        "cargo" => "rustup",
        "npm" => detect_brew_system_method("nvm"),
        "pipx" => detect_brew_system_method("pip"),
        "go" => detect_brew_system_method("dnf"),
        "snap" | "flatpak" => detect_system_method(),
        "nix" => "nix installer",
        _ => "system",
    }
}

/// Plan package actions by diffing installed vs desired for all managers.
/// Handles bootstrap: unavailable managers that can be bootstrapped get Bootstrap
/// actions before their Install actions.
pub fn plan_packages(
    profile: &MergedProfile,
    managers: &[&dyn PackageManager],
) -> Result<Vec<PackageAction>> {
    let mut actions = Vec::new();

    // Pass 1: determine which managers will be bootstrapped
    let mut bootstrapping: HashSet<String> = HashSet::new();
    for manager in managers {
        let desired = cfgd_core::config::desired_packages_for(manager.name(), profile);
        if desired.is_empty() {
            continue;
        }
        if !manager.is_available() && manager.can_bootstrap() {
            bootstrapping.insert(manager.name().to_string());
        }
    }

    // Pass 2: generate actions
    for manager in managers {
        let desired = cfgd_core::config::desired_packages_for(manager.name(), profile);
        if desired.is_empty() {
            continue;
        }

        if manager.is_available() {
            // Normal path: diff installed vs desired
            let installed = manager.installed_packages()?;
            let to_install: Vec<String> = desired
                .iter()
                .filter(|p| !installed.contains(*p))
                .cloned()
                .collect();

            if !to_install.is_empty() {
                actions.push(PackageAction::Install {
                    manager: manager.name().to_string(),
                    packages: to_install,
                    origin: "local".to_string(),
                });
            }
        } else if manager.can_bootstrap() {
            // Unavailable but bootstrappable: add Bootstrap + Install all desired
            actions.push(PackageAction::Bootstrap {
                manager: manager.name().to_string(),
                method: bootstrap_method(*manager).to_string(),
                origin: "local".to_string(),
            });
            actions.push(PackageAction::Install {
                manager: manager.name().to_string(),
                packages: desired,
                origin: "local".to_string(),
            });
        } else if manager
            .name()
            .split('-')
            .next()
            .is_some_and(|prefix| bootstrapping.contains(prefix))
        {
            // Sub-manager whose parent is being bootstrapped (e.g. brew-tap when brew
            // is being bootstrapped). Install all desired — nothing is installed yet.
            actions.push(PackageAction::Install {
                manager: manager.name().to_string(),
                packages: desired,
                origin: "local".to_string(),
            });
        } else {
            actions.push(PackageAction::Skip {
                manager: manager.name().to_string(),
                reason: format!(
                    "'{}' not available — cannot auto-install on this platform",
                    manager.name()
                ),
                origin: "local".to_string(),
            });
        }
    }

    Ok(actions)
}

/// Apply package actions.
#[cfg(test)]
pub fn apply_packages(
    actions: &[PackageAction],
    managers: &[&dyn PackageManager],
    printer: &Printer,
) -> Result<()> {
    for action in actions {
        match action {
            PackageAction::Bootstrap {
                manager: mgr_name, ..
            } => {
                if let Some(mgr) = managers.iter().find(|m| m.name() == mgr_name) {
                    mgr.bootstrap(printer)?;
                }
            }
            PackageAction::Install {
                manager: mgr_name,
                packages,
                ..
            } => {
                if let Some(mgr) = managers.iter().find(|m| m.name() == mgr_name) {
                    mgr.install(packages, printer)?;
                }
            }
            PackageAction::Uninstall {
                manager: mgr_name,
                packages,
                ..
            } => {
                if let Some(mgr) = managers.iter().find(|m| m.name() == mgr_name) {
                    mgr.uninstall(packages, printer)?;
                }
            }
            PackageAction::Skip {
                manager, reason, ..
            } => {
                printer.warning(&format!("{}: {}", manager, reason));
            }
        }
    }

    Ok(())
}

/// Format package actions as human-readable plan items.
#[cfg(test)]
pub fn format_package_actions(actions: &[PackageAction]) -> Vec<String> {
    actions
        .iter()
        .map(|a| match a {
            PackageAction::Bootstrap {
                manager, method, ..
            } => format!("bootstrap {} via {}", manager, method),
            PackageAction::Install {
                manager, packages, ..
            } => format!("install via {}: {}", manager, packages.join(", ")),
            PackageAction::Uninstall {
                manager, packages, ..
            } => format!("uninstall via {}: {}", manager, packages.join(", ")),
            PackageAction::Skip {
                manager, reason, ..
            } => format!("skip {}: {}", manager, reason),
        })
        .collect()
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
                return Err(PackageError::ManagerNotAvailable {
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
                return Err(PackageError::ManagerNotAvailable {
                    manager: manager_name.to_string(),
                }
                .into());
            }
        }
    };
    Ok(removed)
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

// --- Native manifest support ---

/// Parse a Brewfile and extract taps, formulae, and casks.
/// Brewfile format: lines like `tap "name"`, `brew "name"`, `cask "name"`.
/// Comments (#) and blank lines are ignored.
fn parse_brewfile(path: &Path) -> Result<(Vec<String>, Vec<String>, Vec<String>)> {
    let content = std::fs::read_to_string(path).map_err(|e| PackageError::ListFailed {
        manager: "brew".into(),
        message: format!("failed to read Brewfile {}: {}", path.display(), e),
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
        message: format!("failed to read apt manifest {}: {}", path.display(), e),
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
        message: format!("failed to read package.json {}: {}", path.display(), e),
    })?;

    let json: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| PackageError::ListFailed {
            manager: "npm".into(),
            message: format!("failed to parse package.json {}: {}", path.display(), e),
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
        message: format!("failed to read Cargo.toml {}: {}", path.display(), e),
    })?;

    let toml_val: toml::Value = toml::from_str(&content).map_err(|e| PackageError::ListFailed {
        manager: "cargo".into(),
        message: format!("failed to parse Cargo.toml {}: {}", path.display(), e),
    })?;

    let mut packages = Vec::new();

    if let Some(deps) = toml_val.get("dependencies").and_then(|v| v.as_table()) {
        for key in deps.keys() {
            packages.push(key.clone());
        }
    }

    Ok(packages)
}

/// Resolve manifest files referenced in package specs and merge their contents
/// into the inline package lists. Paths are relative to `config_dir`.
pub fn resolve_manifest_packages(packages: &mut PackagesSpec, config_dir: &Path) -> Result<()> {
    // Brew: parse Brewfile, merge taps/formulae/casks
    if let Some(ref mut brew) = packages.brew
        && let Some(ref file) = brew.file
    {
        let path = config_dir.join(file);
        if path.exists() {
            let (taps, formulae, casks) = parse_brewfile(&path)?;
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
            let pkgs = parse_apt_manifest(&path)?;
            cfgd_core::union_extend(&mut apt.packages, &pkgs);
        }
    }

    // Npm: parse package.json
    if let Some(ref mut npm) = packages.npm
        && let Some(ref file) = npm.file
    {
        let path = config_dir.join(file);
        if path.exists() {
            let pkgs = parse_npm_package_json(&path)?;
            cfgd_core::union_extend(&mut npm.global, &pkgs);
        }
    }

    // Cargo: parse Cargo.toml
    if let Some(ref mut cargo) = packages.cargo
        && let Some(ref file) = cargo.file
    {
        let path = config_dir.join(file);
        if path.exists() {
            let pkgs = parse_cargo_toml(&path)?;
            cfgd_core::union_extend(&mut cargo.packages, &pkgs);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests;
