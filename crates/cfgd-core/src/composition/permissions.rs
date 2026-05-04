use std::collections::HashMap;

use crate::config::PolicyItems;

use super::CompositionInput;

/// Describes a permission-expanding change detected between old and new composition results.
#[derive(Debug, Clone)]
pub struct PermissionChange {
    pub source: String,
    pub description: String,
}

/// Compare old vs new composition results to detect permission-expanding changes.
/// Returns a list of changes that require explicit user consent.
pub fn detect_permission_changes(
    old_sources: &[CompositionInput],
    new_sources: &[CompositionInput],
) -> Vec<PermissionChange> {
    let mut changes = Vec::new();

    let old_map: HashMap<&str, &CompositionInput> = old_sources
        .iter()
        .map(|s| (s.source_name.as_str(), s))
        .collect();

    for new_src in new_sources {
        let name = &new_src.source_name;
        match old_map.get(name.as_str()) {
            None => {
                changes.push(PermissionChange {
                    source: name.clone(),
                    description: "New source added".to_string(),
                });
            }
            Some(old_src) => {
                // Check if locked items increased
                let old_locked = count_policy_tier_items(&old_src.policy.locked);
                let new_locked = count_policy_tier_items(&new_src.policy.locked);
                if new_locked > old_locked {
                    changes.push(PermissionChange {
                        source: name.clone(),
                        description: format!(
                            "Locked items increased from {} to {}",
                            old_locked, new_locked
                        ),
                    });
                }

                // Check if required items increased
                let old_required = count_policy_tier_items(&old_src.policy.required);
                let new_required = count_policy_tier_items(&new_src.policy.required);
                if new_required > old_required {
                    changes.push(PermissionChange {
                        source: name.clone(),
                        description: format!(
                            "Required items increased from {} to {}",
                            old_required, new_required
                        ),
                    });
                }

                // Check if constraints relaxed (scripts enabled, paths expanded)
                let old_c = &old_src.constraints;
                let new_c = &new_src.constraints;
                if old_c.no_scripts && !new_c.no_scripts {
                    changes.push(PermissionChange {
                        source: name.clone(),
                        description: "Scripts have been enabled".to_string(),
                    });
                }
                if new_c.allowed_target_paths.len() > old_c.allowed_target_paths.len() {
                    changes.push(PermissionChange {
                        source: name.clone(),
                        description: "Allowed target paths expanded".to_string(),
                    });
                }
            }
        }
    }

    changes
}

pub(super) fn count_policy_tier_items(items: &PolicyItems) -> usize {
    let mut count = 0;
    if let Some(ref pkgs) = items.packages {
        if let Some(ref brew) = pkgs.brew {
            count += brew.formulae.len() + brew.casks.len() + brew.taps.len();
        }
        if let Some(ref apt) = pkgs.apt {
            count += apt.packages.len();
        }
        if let Some(ref cargo) = pkgs.cargo {
            count += cargo.packages.len();
        }
        count += pkgs.pipx.len() + pkgs.dnf.len();
        if let Some(ref npm) = pkgs.npm {
            count += npm.global.len();
        }
    }
    count += items.files.len();
    count += items.env.len();
    count += items.aliases.len();
    count += items.system.len();
    count += items.modules.len();
    count
}
