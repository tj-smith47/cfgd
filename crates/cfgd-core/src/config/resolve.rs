use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;

use super::parse::{find_profile_path, load_profile};
use super::profile_spec::{
    EnvScope, FilesSpec, PackagesSpec, ProfileSpec, ScriptSpec, SecretSpec, validate_secret_specs,
};
use super::source::{EnvVar, ShellAlias};
use crate::errors::{ConfigError, Result};
use crate::{deep_merge_yaml, union_extend};

// --- Profile Resolution ---

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum LayerPolicy {
    Local,
    Required,
    Recommended,
    Optional,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProfileLayer {
    pub source: String,
    pub profile_name: String,
    pub priority: u32,
    pub policy: LayerPolicy,
    pub spec: ProfileSpec,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolvedProfile {
    pub layers: Vec<ProfileLayer>,
    pub merged: MergedProfile,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct MergedProfile {
    pub modules: Vec<String>,
    pub env: Vec<EnvVar>,
    pub env_scope: EnvScope,
    pub aliases: Vec<ShellAlias>,
    pub packages: PackagesSpec,
    pub files: FilesSpec,
    pub system: HashMap<String, serde_yaml::Value>,
    pub secrets: Vec<SecretSpec>,
    pub scripts: ScriptSpec,
}

/// Resolve a profile by loading it and its full inheritance chain, then merging.
pub fn resolve_profile(profile_name: &str, profiles_dir: &Path) -> Result<ResolvedProfile> {
    let resolution_order = resolve_inheritance_order(profile_name, profiles_dir, &mut vec![])?;

    let mut layers = Vec::new();
    for name in &resolution_order {
        let path = find_profile_path(profiles_dir, name);
        let doc = load_profile(&path).map_err(|e| match e {
            crate::errors::CfgdError::Config(ConfigError::NotFound { .. }) => {
                crate::errors::CfgdError::Config(ConfigError::ProfileNotFound {
                    name: name.clone(),
                })
            }
            other => other,
        })?;
        layers.push(ProfileLayer {
            source: "local".to_string(),
            profile_name: name.clone(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: doc.spec,
        });
    }

    let merged = merge_layers(&layers);

    validate_secret_specs(&merged.secrets)?;

    Ok(ResolvedProfile { layers, merged })
}

/// Recursively resolve the inheritance order (depth-first, left-to-right).
/// Returns profiles in resolution order: earliest ancestor first, active profile last.
fn resolve_inheritance_order(
    profile_name: &str,
    profiles_dir: &Path,
    visited: &mut Vec<String>,
) -> Result<Vec<String>> {
    if visited.contains(&profile_name.to_string()) {
        let mut chain = visited.clone();
        chain.push(profile_name.to_string());
        return Err(ConfigError::CircularInheritance { chain }.into());
    }

    visited.push(profile_name.to_string());

    let path = find_profile_path(profiles_dir, profile_name);
    let doc = load_profile(&path).map_err(|e| match e {
        crate::errors::CfgdError::Config(ConfigError::NotFound { .. }) => {
            crate::errors::CfgdError::Config(ConfigError::ProfileNotFound {
                name: profile_name.to_string(),
            })
        }
        other => other,
    })?;

    let mut order = Vec::new();
    for parent in &doc.spec.inherits {
        let parent_order = resolve_inheritance_order(parent, profiles_dir, visited)?;
        for name in parent_order {
            if !order.contains(&name) {
                order.push(name);
            }
        }
    }

    order.push(profile_name.to_string());
    visited.pop();

    Ok(order)
}

/// Merge profile layers according to merge rules:
/// - packages: union
/// - files: overlay (later overrides earlier for same target)
/// - env: override (later replaces earlier for same name)
/// - secrets: append (deduplicated by target)
/// - scripts: append in order
/// - system: deep merge (later overrides at leaf level)
pub(super) fn merge_layers(layers: &[ProfileLayer]) -> MergedProfile {
    let mut merged = MergedProfile::default();

    for layer in layers {
        let spec = &layer.spec;

        // Modules: union
        union_extend(&mut merged.modules, &spec.modules);

        // Env: later layer overrides earlier by name
        crate::merge_env(&mut merged.env, &spec.env);

        // EnvScope: last layer that *specifies* it wins; an omitting layer
        // inherits the value resolved so far (defaults to All if none set it).
        if let Some(scope) = spec.env_scope {
            merged.env_scope = scope;
        }

        // Aliases: later layer overrides earlier by name
        crate::merge_aliases(&mut merged.aliases, &spec.aliases);

        // Packages: union (delegated to composition::merge_packages)
        if let Some(ref pkgs) = spec.packages {
            crate::composition::merge_packages(&mut merged.packages, pkgs);
        }

        // Files: overlay (later layer overrides earlier for same target)
        if let Some(ref files) = spec.files {
            for managed in &files.managed {
                if let Some(existing) = merged
                    .files
                    .managed
                    .iter_mut()
                    .find(|m| m.target == managed.target)
                {
                    *existing = managed.clone();
                } else {
                    merged.files.managed.push(managed.clone());
                }
            }
            for (path, mode) in &files.permissions {
                merged.files.permissions.insert(path.clone(), mode.clone());
            }
        }

        // System: deep merge at leaf level
        for (key, value) in &spec.system {
            deep_merge_yaml(
                merged
                    .system
                    .entry(key.clone())
                    .or_insert(serde_yaml::Value::Null),
                value,
            );
        }

        // Secrets: append, deduplicate by source (later layer overrides)
        for secret in &spec.secrets {
            if let Some(existing) = merged
                .secrets
                .iter_mut()
                .find(|s| s.source == secret.source)
            {
                *existing = secret.clone();
            } else {
                merged.secrets.push(secret.clone());
            }
        }

        // Scripts: append in order
        if let Some(ref scripts) = spec.scripts {
            merged.scripts.pre_apply.extend(scripts.pre_apply.clone());
            merged.scripts.post_apply.extend(scripts.post_apply.clone());
            merged
                .scripts
                .pre_reconcile
                .extend(scripts.pre_reconcile.clone());
            merged
                .scripts
                .post_reconcile
                .extend(scripts.post_reconcile.clone());
            merged.scripts.on_drift.extend(scripts.on_drift.clone());
            merged.scripts.on_change.extend(scripts.on_change.clone());
        }
    }

    merged
}

/// Get the list of desired packages for a specific package manager from a merged profile.
pub fn desired_packages_for(manager_name: &str, profile: &MergedProfile) -> Vec<String> {
    desired_packages_for_spec(manager_name, &profile.packages)
}

pub fn desired_packages_for_spec(manager_name: &str, packages: &PackagesSpec) -> Vec<String> {
    match manager_name {
        "brew" => packages
            .brew
            .as_ref()
            .map(|b| b.formulae.clone())
            .unwrap_or_default(),
        "brew-tap" => packages
            .brew
            .as_ref()
            .map(|b| b.taps.clone())
            .unwrap_or_default(),
        "brew-cask" => packages
            .brew
            .as_ref()
            .map(|b| b.casks.clone())
            .unwrap_or_default(),
        "apt" => packages
            .apt
            .as_ref()
            .map(|a| a.packages.clone())
            .unwrap_or_default(),
        "cargo" => packages
            .cargo
            .as_ref()
            .map(|c| c.packages.clone())
            .unwrap_or_default(),
        "npm" => packages
            .npm
            .as_ref()
            .map(|n| n.global.clone())
            .unwrap_or_default(),
        "pipx" => packages.pipx.clone(),
        "dnf" => packages.dnf.clone(),
        "apk" => packages.apk.clone(),
        "pacman" => packages.pacman.clone(),
        "zypper" => packages.zypper.clone(),
        "yum" => packages.yum.clone(),
        "pkg" => packages.pkg.clone(),
        "snap" => packages
            .snap
            .as_ref()
            .map(|s| {
                let mut all = s.packages.clone();
                for p in &s.classic {
                    if !all.contains(p) {
                        all.push(p.clone());
                    }
                }
                all
            })
            .unwrap_or_default(),
        "flatpak" => packages
            .flatpak
            .as_ref()
            .map(|f| f.packages.clone())
            .unwrap_or_default(),
        "nix" => packages.nix.clone(),
        "go" => packages.go.clone(),
        "winget" => packages.winget.clone(),
        "chocolatey" => packages.chocolatey.clone(),
        "scoop" => packages.scoop.clone(),
        _ => {
            // Check custom managers
            for custom in &packages.custom {
                if custom.name == manager_name {
                    return custom.packages.clone();
                }
            }
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(name: &str, env_scope: Option<EnvScope>) -> ProfileLayer {
        ProfileLayer {
            source: "local".to_string(),
            profile_name: name.to_string(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec {
                env_scope,
                ..Default::default()
            },
        }
    }

    #[test]
    fn env_scope_defaults_to_all_when_no_layer_sets_it() {
        let merged = merge_layers(&[layer("base", None), layer("child", None)]);
        assert_eq!(merged.env_scope, EnvScope::All);
    }

    #[test]
    fn env_scope_child_omitting_inherits_parent_value() {
        // base sets Interactive; child omits — the parent's choice must survive.
        let merged = merge_layers(&[
            layer("base", Some(EnvScope::Interactive)),
            layer("child", None),
        ]);
        assert_eq!(merged.env_scope, EnvScope::Interactive);
    }

    #[test]
    fn env_scope_child_specifying_overrides_parent() {
        let merged = merge_layers(&[
            layer("base", Some(EnvScope::Interactive)),
            layer("child", Some(EnvScope::Login)),
        ]);
        assert_eq!(merged.env_scope, EnvScope::Login);
    }
}
