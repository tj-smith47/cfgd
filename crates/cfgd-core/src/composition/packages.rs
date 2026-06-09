use crate::config::{MergeSpec, PackagesSpec, PolicyItems};
use crate::errors::{CompositionError, Result};
use crate::union_extend;

/// Top-level keys honored by [`filter_rejected`] (and `record::record_rejections`).
/// Any other top-level key in a `reject:` mapping is a typo that would silently
/// fail to reject — so it is rejected at composition time.
const ALLOWED_REJECT_KEYS: [&str; 4] = ["packages", "env", "aliases", "modules"];

/// Validate a subscriber's `reject` value. A non-null mapping must use only the
/// keys [`filter_rejected`] understands; anything else is a typo that would let
/// the unwanted item through unnoticed.
pub(super) fn validate_reject_keys(source_name: &str, reject: &serde_yaml::Value) -> Result<()> {
    let Some(map) = reject.as_mapping() else {
        return Ok(());
    };
    for key in map.keys() {
        let Some(key_str) = key.as_str() else {
            return Err(CompositionError::InvalidReject {
                source_name: source_name.to_string(),
                key: format!("{key:?}"),
            }
            .into());
        };
        if !ALLOWED_REJECT_KEYS.contains(&key_str) {
            return Err(CompositionError::InvalidReject {
                source_name: source_name.to_string(),
                key: key_str.to_string(),
            }
            .into());
        }
    }
    Ok(())
}

/// Layer `source`'s optional manager spec onto `target`'s. When `source` is
/// `Some`, the target field is materialized (default if absent) and the
/// incoming values are layered on via the type's [`MergeSpec`] impl. The
/// per-field policy lives on the spec type, never here, so this stays uniform
/// across every struct-backed manager.
fn merge_optional<T: MergeSpec + Default>(target: &mut Option<T>, source: &Option<T>) {
    if let Some(incoming) = source {
        target
            .get_or_insert_with(Default::default)
            .merge_from(incoming);
    }
}

/// Merge packages from `source` into `target`, unioning lists and applying
/// later-wins for scalar fields (file paths, remotes, custom manager commands).
///
/// Per-manager merge semantics live on each spec type's [`MergeSpec`] impl, so
/// this function holds no per-field logic: every manager is a uniform call and
/// adding a field to a spec cannot silently drift the merge layer.
pub fn merge_packages(target: &mut PackagesSpec, source: &PackagesSpec) {
    merge_optional(&mut target.brew, &source.brew);
    merge_optional(&mut target.apt, &source.apt);
    merge_optional(&mut target.cargo, &source.cargo);
    merge_optional(&mut target.npm, &source.npm);
    merge_optional(&mut target.snap, &source.snap);
    merge_optional(&mut target.flatpak, &source.flatpak);
    union_extend(&mut target.pipx, &source.pipx);
    union_extend(&mut target.dnf, &source.dnf);
    union_extend(&mut target.apk, &source.apk);
    union_extend(&mut target.pacman, &source.pacman);
    union_extend(&mut target.zypper, &source.zypper);
    union_extend(&mut target.yum, &source.yum);
    union_extend(&mut target.pkg, &source.pkg);
    union_extend(&mut target.nix, &source.nix);
    union_extend(&mut target.go, &source.go);
    union_extend(&mut target.winget, &source.winget);
    union_extend(&mut target.chocolatey, &source.chocolatey);
    union_extend(&mut target.scoop, &source.scoop);
    // Custom managers: merge by name (existing entry layered via MergeSpec),
    // append otherwise. Name is the merge key, so it is never overwritten.
    for custom in &source.custom {
        if let Some(existing) = target.custom.iter_mut().find(|c| c.name == custom.name) {
            existing.merge_from(custom);
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

#[cfg(test)]
mod tests {
    use crate::config::{BrewSpec, CustomManagerSpec, FlatpakSpec, PackagesSpec, SnapSpec};

    use super::merge_packages;

    fn brew(file: Option<&str>, formulae: &[&str], taps: &[&str], casks: &[&str]) -> BrewSpec {
        BrewSpec {
            file: file.map(str::to_string),
            taps: taps.iter().map(|s| s.to_string()).collect(),
            formulae: formulae.iter().map(|s| s.to_string()).collect(),
            casks: casks.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn struct_scalar_field_overwrites_when_source_some() {
        let mut target = PackagesSpec {
            brew: Some(brew(Some("a/Brewfile"), &[], &[], &[])),
            ..Default::default()
        };
        let source = PackagesSpec {
            brew: Some(brew(Some("b/Brewfile"), &[], &[], &[])),
            ..Default::default()
        };
        merge_packages(&mut target, &source);
        assert_eq!(target.brew.unwrap().file.as_deref(), Some("b/Brewfile"));
    }

    #[test]
    fn struct_scalar_field_kept_when_source_none() {
        let mut target = PackagesSpec {
            brew: Some(brew(Some("a/Brewfile"), &[], &[], &[])),
            ..Default::default()
        };
        let source = PackagesSpec {
            brew: Some(brew(None, &["jq"], &[], &[])),
            ..Default::default()
        };
        merge_packages(&mut target, &source);
        let result = target.brew.unwrap();
        assert_eq!(result.file.as_deref(), Some("a/Brewfile"));
        assert_eq!(result.formulae, vec!["jq".to_string()]);
    }

    #[test]
    fn struct_list_fields_union_dedup_order_preserved() {
        let mut target = PackagesSpec {
            brew: Some(brew(None, &["jq", "ripgrep"], &["a/tap"], &["firefox"])),
            ..Default::default()
        };
        let source = PackagesSpec {
            brew: Some(brew(
                None,
                &["ripgrep", "bat"],
                &["a/tap", "b/tap"],
                &["firefox", "chrome"],
            )),
            ..Default::default()
        };
        merge_packages(&mut target, &source);
        let result = target.brew.unwrap();
        assert_eq!(result.formulae, vec!["jq", "ripgrep", "bat"]);
        assert_eq!(result.taps, vec!["a/tap", "b/tap"]);
        assert_eq!(result.casks, vec!["firefox", "chrome"]);
    }

    #[test]
    fn struct_manager_inserted_when_target_none() {
        let mut target = PackagesSpec::default();
        let source = PackagesSpec {
            brew: Some(brew(Some("Brewfile"), &["jq"], &["x/tap"], &["app"])),
            ..Default::default()
        };
        merge_packages(&mut target, &source);
        let result = target.brew.expect("brew inserted from source");
        assert_eq!(result.file.as_deref(), Some("Brewfile"));
        assert_eq!(result.formulae, vec!["jq".to_string()]);
        assert_eq!(result.taps, vec!["x/tap".to_string()]);
        assert_eq!(result.casks, vec!["app".to_string()]);
    }

    #[test]
    fn snap_dual_list_fields_union_dedup() {
        let mut target = PackagesSpec {
            snap: Some(SnapSpec {
                packages: vec!["a".into(), "b".into()],
                classic: vec!["code".into()],
            }),
            ..Default::default()
        };
        let source = PackagesSpec {
            snap: Some(SnapSpec {
                packages: vec!["b".into(), "c".into()],
                classic: vec!["code".into(), "go".into()],
            }),
            ..Default::default()
        };
        merge_packages(&mut target, &source);
        let result = target.snap.unwrap();
        assert_eq!(result.packages, vec!["a", "b", "c"]);
        assert_eq!(result.classic, vec!["code", "go"]);
    }

    #[test]
    fn flatpak_scalar_remote_overwrites_list_unions() {
        let mut target = PackagesSpec {
            flatpak: Some(FlatpakSpec {
                packages: vec!["org.gimp.GIMP".into()],
                remote: Some("flathub".into()),
            }),
            ..Default::default()
        };
        let source = PackagesSpec {
            flatpak: Some(FlatpakSpec {
                packages: vec!["org.gimp.GIMP".into(), "org.inkscape.Inkscape".into()],
                remote: Some("flathub-beta".into()),
            }),
            ..Default::default()
        };
        merge_packages(&mut target, &source);
        let result = target.flatpak.unwrap();
        assert_eq!(
            result.packages,
            vec!["org.gimp.GIMP", "org.inkscape.Inkscape"]
        );
        assert_eq!(result.remote.as_deref(), Some("flathub-beta"));
    }

    #[test]
    fn bare_list_manager_unions_dedup() {
        let mut target = PackagesSpec {
            pipx: vec!["black".into(), "ruff".into()],
            ..Default::default()
        };
        let source = PackagesSpec {
            pipx: vec!["ruff".into(), "mypy".into()],
            ..Default::default()
        };
        merge_packages(&mut target, &source);
        assert_eq!(target.pipx, vec!["black", "ruff", "mypy"]);
    }

    #[test]
    fn custom_manager_merges_by_name() {
        let base = || CustomManagerSpec {
            name: "asdf".into(),
            check: "asdf which".into(),
            list_installed: "asdf list".into(),
            install: "asdf install".into(),
            uninstall: "asdf uninstall".into(),
            update: Some("asdf update old".into()),
            packages: vec!["nodejs".into()],
        };
        let mut target = PackagesSpec {
            custom: vec![base()],
            ..Default::default()
        };
        let source = PackagesSpec {
            custom: vec![CustomManagerSpec {
                update: Some("asdf update new".into()),
                packages: vec!["nodejs".into(), "python".into()],
                ..base()
            }],
            ..Default::default()
        };
        merge_packages(&mut target, &source);
        assert_eq!(target.custom.len(), 1);
        let merged = &target.custom[0];
        assert_eq!(merged.name, "asdf");
        assert_eq!(merged.update.as_deref(), Some("asdf update new"));
        assert_eq!(merged.packages, vec!["nodejs", "python"]);
    }

    #[test]
    fn custom_manager_appended_when_new_name() {
        let mut target = PackagesSpec {
            custom: vec![CustomManagerSpec {
                name: "asdf".into(),
                check: "c".into(),
                list_installed: "l".into(),
                install: "i".into(),
                uninstall: "u".into(),
                update: None,
                packages: vec![],
            }],
            ..Default::default()
        };
        let source = PackagesSpec {
            custom: vec![CustomManagerSpec {
                name: "mise".into(),
                check: "c2".into(),
                list_installed: "l2".into(),
                install: "i2".into(),
                uninstall: "u2".into(),
                update: None,
                packages: vec!["node".into()],
            }],
            ..Default::default()
        };
        merge_packages(&mut target, &source);
        assert_eq!(target.custom.len(), 2);
        assert_eq!(target.custom[1].name, "mise");
    }

    #[test]
    fn custom_update_none_keeps_existing() {
        let mut target = PackagesSpec {
            custom: vec![CustomManagerSpec {
                name: "asdf".into(),
                check: "c".into(),
                list_installed: "l".into(),
                install: "i".into(),
                uninstall: "u".into(),
                update: Some("keep me".into()),
                packages: vec![],
            }],
            ..Default::default()
        };
        let source = PackagesSpec {
            custom: vec![CustomManagerSpec {
                name: "asdf".into(),
                check: "c".into(),
                list_installed: "l".into(),
                install: "i".into(),
                uninstall: "u".into(),
                update: None,
                packages: vec![],
            }],
            ..Default::default()
        };
        merge_packages(&mut target, &source);
        assert_eq!(target.custom[0].update.as_deref(), Some("keep me"));
    }
}
