use crate::config::{PackagesSpec, PolicyItems};

use super::{ConflictResolution, ResolutionType};

pub(super) fn record_policy_conflicts(
    source_name: &str,
    items: &PolicyItems,
    resolution_type: ResolutionType,
    conflicts: &mut Vec<ConflictResolution>,
) {
    // Record file conflicts
    for file in &items.files {
        conflicts.push(ConflictResolution {
            resource_id: file.target.to_string_lossy().to_string(),
            resolution_type: resolution_type.clone(),
            winning_source: source_name.to_string(),
            details: format!(
                "{} {} <- {}",
                resolution_type.label(),
                file.target.display(),
                source_name
            ),
        });
    }

    // Record package conflicts
    if let Some(ref pkgs) = items.packages {
        let all_packages = collect_package_names(pkgs);
        for pkg in all_packages {
            conflicts.push(ConflictResolution {
                resource_id: pkg.clone(),
                resolution_type: resolution_type.clone(),
                winning_source: source_name.to_string(),
                details: format!("{} {} <- {}", resolution_type.label(), pkg, source_name),
            });
        }
    }

    // Record env conflicts
    for ev in &items.env {
        conflicts.push(ConflictResolution {
            resource_id: format!("env:{}", ev.name),
            resolution_type: resolution_type.clone(),
            winning_source: source_name.to_string(),
            details: format!("{} {} <- {}", resolution_type.label(), ev.name, source_name),
        });
    }

    // Record alias conflicts
    for alias in &items.aliases {
        conflicts.push(ConflictResolution {
            resource_id: format!("alias:{}", alias.name),
            resolution_type: resolution_type.clone(),
            winning_source: source_name.to_string(),
            details: format!(
                "{} {} <- {}",
                resolution_type.label(),
                alias.name,
                source_name
            ),
        });
    }

    // Record module conflicts
    for module in &items.modules {
        conflicts.push(ConflictResolution {
            resource_id: format!("module:{}", module),
            resolution_type: resolution_type.clone(),
            winning_source: source_name.to_string(),
            details: format!(
                "{} module {} <- {}",
                resolution_type.label(),
                module,
                source_name
            ),
        });
    }

    // Record secret conflicts
    for secret in &items.secrets {
        conflicts.push(ConflictResolution {
            resource_id: format!("secret:{}", secret.source),
            resolution_type: resolution_type.clone(),
            winning_source: source_name.to_string(),
            details: format!(
                "{} {} <- {}",
                resolution_type.label(),
                secret.source,
                source_name
            ),
        });
    }
}

pub(super) fn record_rejections(
    source_name: &str,
    _recommended: &PolicyItems,
    reject: &serde_yaml::Value,
    conflicts: &mut Vec<ConflictResolution>,
) {
    if reject.is_null() {
        return;
    }

    if let Some(reject_map) = reject.as_mapping()
        && let Some(pkg_val) = reject_map.get(serde_yaml::Value::String("packages".into()))
        && let Some(pkg_map) = pkg_val.as_mapping()
    {
        for (_, list) in pkg_map {
            if let Some(seq) = list.as_sequence() {
                for item in seq {
                    if let Some(name) = item.as_str() {
                        conflicts.push(ConflictResolution {
                            resource_id: name.to_string(),
                            resolution_type: ResolutionType::Rejected,
                            winning_source: "local".to_string(),
                            details: format!(
                                "REJECTED {} <- local rejected {} recommendation",
                                name, source_name
                            ),
                        });
                    }
                }
            }
            // Handle mapping-style rejection (e.g. brew: {formulae: [x]})
            if let Some(sub_map) = list.as_mapping() {
                for (_, sub_list) in sub_map {
                    if let Some(seq) = sub_list.as_sequence() {
                        for item in seq {
                            if let Some(name) = item.as_str() {
                                conflicts.push(ConflictResolution {
                                    resource_id: name.to_string(),
                                    resolution_type: ResolutionType::Rejected,
                                    winning_source: "local".to_string(),
                                    details: format!(
                                        "REJECTED {} <- local rejected {} recommendation",
                                        name, source_name
                                    ),
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    // Record rejected modules
    if let Some(reject_map) = reject.as_mapping()
        && let Some(mod_val) = reject_map.get(serde_yaml::Value::String("modules".into()))
        && let Some(mod_seq) = mod_val.as_sequence()
    {
        for item in mod_seq {
            if let Some(name) = item.as_str() {
                conflicts.push(ConflictResolution {
                    resource_id: format!("module:{}", name),
                    resolution_type: ResolutionType::Rejected,
                    winning_source: "local".to_string(),
                    details: format!(
                        "REJECTED module {} <- local rejected {} recommendation",
                        name, source_name
                    ),
                });
            }
        }
    }
}

pub(super) fn collect_package_names(pkgs: &PackagesSpec) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(ref brew) = pkgs.brew {
        for f in &brew.formulae {
            names.push(format!("{} (brew)", f));
        }
        for c in &brew.casks {
            names.push(format!("{} (brew cask)", c));
        }
    }
    if let Some(ref apt) = pkgs.apt {
        for p in &apt.packages {
            names.push(format!("{} (apt)", p));
        }
    }
    if let Some(ref cargo) = pkgs.cargo {
        for p in &cargo.packages {
            names.push(format!("{} (cargo)", p));
        }
    }
    if let Some(ref npm) = pkgs.npm {
        for p in &npm.global {
            names.push(format!("{} (npm)", p));
        }
    }
    for p in &pkgs.pipx {
        names.push(format!("{} (pipx)", p));
    }
    for p in &pkgs.dnf {
        names.push(format!("{} (dnf)", p));
    }
    names
}
