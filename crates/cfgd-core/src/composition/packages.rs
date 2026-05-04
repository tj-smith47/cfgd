use crate::config::{PackagesSpec, PolicyItems};
use crate::union_extend;

/// Merge packages from `source` into `target`, unioning lists and applying
/// later-wins for scalar fields (file paths, remotes, custom manager commands).
pub fn merge_packages(target: &mut PackagesSpec, source: &PackagesSpec) {
    if let Some(ref brew) = source.brew {
        let target_brew = target.brew.get_or_insert_with(Default::default);
        if brew.file.is_some() {
            target_brew.file = brew.file.clone();
        }
        union_extend(&mut target_brew.taps, &brew.taps);
        union_extend(&mut target_brew.formulae, &brew.formulae);
        union_extend(&mut target_brew.casks, &brew.casks);
    }
    if let Some(ref apt) = source.apt {
        let target_apt = target.apt.get_or_insert_with(Default::default);
        if apt.file.is_some() {
            target_apt.file = apt.file.clone();
        }
        union_extend(&mut target_apt.packages, &apt.packages);
    }
    if let Some(ref cargo) = source.cargo {
        let target_cargo = target.cargo.get_or_insert_with(Default::default);
        if cargo.file.is_some() {
            target_cargo.file = cargo.file.clone();
        }
        union_extend(&mut target_cargo.packages, &cargo.packages);
    }
    if let Some(ref npm) = source.npm {
        let target_npm = target.npm.get_or_insert_with(Default::default);
        if npm.file.is_some() {
            target_npm.file = npm.file.clone();
        }
        union_extend(&mut target_npm.global, &npm.global);
    }
    union_extend(&mut target.pipx, &source.pipx);
    union_extend(&mut target.dnf, &source.dnf);
    union_extend(&mut target.apk, &source.apk);
    union_extend(&mut target.pacman, &source.pacman);
    union_extend(&mut target.zypper, &source.zypper);
    union_extend(&mut target.yum, &source.yum);
    union_extend(&mut target.pkg, &source.pkg);
    if let Some(ref snap) = source.snap {
        let target_snap = target.snap.get_or_insert_with(Default::default);
        union_extend(&mut target_snap.packages, &snap.packages);
        union_extend(&mut target_snap.classic, &snap.classic);
    }
    if let Some(ref flatpak) = source.flatpak {
        let target_flatpak = target.flatpak.get_or_insert_with(Default::default);
        union_extend(&mut target_flatpak.packages, &flatpak.packages);
        if flatpak.remote.is_some() {
            target_flatpak.remote = flatpak.remote.clone();
        }
    }
    union_extend(&mut target.nix, &source.nix);
    union_extend(&mut target.go, &source.go);
    union_extend(&mut target.winget, &source.winget);
    union_extend(&mut target.chocolatey, &source.chocolatey);
    union_extend(&mut target.scoop, &source.scoop);
    // Custom managers: merge by name, union packages
    for custom in &source.custom {
        if let Some(existing) = target.custom.iter_mut().find(|c| c.name == custom.name) {
            existing.check = custom.check.clone();
            existing.list_installed = custom.list_installed.clone();
            existing.install = custom.install.clone();
            existing.uninstall = custom.uninstall.clone();
            if custom.update.is_some() {
                existing.update = custom.update.clone();
            }
            union_extend(&mut existing.packages, &custom.packages);
        } else {
            target.custom.push(custom.clone());
        }
    }
}

/// Filter rejected items from recommended policy items.
pub(super) fn filter_rejected(
    recommended: &PolicyItems,
    reject: &serde_yaml::Value,
) -> PolicyItems {
    if reject.is_null() {
        return recommended.clone();
    }

    let mut filtered = recommended.clone();

    // Filter rejected packages
    if let Some(reject_map) = reject.as_mapping() {
        if let Some(pkg_val) = reject_map.get(serde_yaml::Value::String("packages".into()))
            && let Some(ref mut pkgs) = filtered.packages
        {
            filter_rejected_packages(pkgs, pkg_val);
        }

        // Filter rejected env
        if let Some(env_val) = reject_map.get(serde_yaml::Value::String("env".into()))
            && let Some(env_map) = env_val.as_mapping()
        {
            for (key, _) in env_map {
                if let Some(key_str) = key.as_str() {
                    filtered.env.retain(|e| e.name != key_str);
                }
            }
        }

        // Filter rejected aliases
        if let Some(alias_val) = reject_map.get(serde_yaml::Value::String("aliases".into()))
            && let Some(alias_map) = alias_val.as_mapping()
        {
            for (key, _) in alias_map {
                if let Some(key_str) = key.as_str() {
                    filtered.aliases.retain(|a| a.name != key_str);
                }
            }
        }

        // Filter rejected modules
        if let Some(mod_val) = reject_map.get(serde_yaml::Value::String("modules".into()))
            && let Some(mod_seq) = mod_val.as_sequence()
        {
            let rejected: Vec<String> = mod_seq
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
            filtered.modules.retain(|m| !rejected.contains(m));
        }
    }

    filtered
}

fn filter_rejected_packages(packages: &mut PackagesSpec, reject: &serde_yaml::Value) {
    if let Some(reject_map) = reject.as_mapping() {
        if let Some(brew_val) = reject_map.get(serde_yaml::Value::String("brew".into()))
            && let Some(ref mut brew) = packages.brew
            && let Some(brew_map) = brew_val.as_mapping()
        {
            remove_rejected_list(
                &mut brew.formulae,
                brew_map.get(serde_yaml::Value::String("formulae".into())),
            );
            remove_rejected_list(
                &mut brew.casks,
                brew_map.get(serde_yaml::Value::String("casks".into())),
            );
            remove_rejected_list(
                &mut brew.taps,
                brew_map.get(serde_yaml::Value::String("taps".into())),
            );
        }
        // Similar for other package managers
        remove_rejected_from_mapping(reject_map, "apt", |val| {
            if let Some(ref mut apt) = packages.apt
                && let Some(apt_map) = val.as_mapping()
            {
                remove_rejected_list(
                    &mut apt.packages,
                    apt_map.get(serde_yaml::Value::String("packages".into())),
                );
            }
        });
        if let Some(ref mut cargo) = packages.cargo {
            remove_rejected_from_seq(reject_map, "cargo", &mut cargo.packages);
        }
        remove_rejected_from_seq(reject_map, "pipx", &mut packages.pipx);
        remove_rejected_from_seq(reject_map, "dnf", &mut packages.dnf);
    }
}

fn remove_rejected_list(target: &mut Vec<String>, reject: Option<&serde_yaml::Value>) {
    if let Some(val) = reject
        && let Some(seq) = val.as_sequence()
    {
        let rejected: Vec<String> = seq
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
        target.retain(|item| !rejected.contains(item));
    }
}

fn remove_rejected_from_mapping(
    reject_map: &serde_yaml::Mapping,
    key: &str,
    f: impl FnOnce(&serde_yaml::Value),
) {
    if let Some(val) = reject_map.get(serde_yaml::Value::String(key.into())) {
        f(val);
    }
}

fn remove_rejected_from_seq(reject_map: &serde_yaml::Mapping, key: &str, target: &mut Vec<String>) {
    if let Some(val) = reject_map.get(serde_yaml::Value::String(key.into())) {
        remove_rejected_list(target, Some(val));
    }
}
