use std::collections::HashMap;

use crate::PathDisplayExt;
use crate::config::{LayerPolicy, MergedProfile, ProfileLayer};
use crate::errors::CompositionError;
use crate::{deep_merge_yaml, union_extend};

use super::layers::FileOwner;
use super::packages::merge_packages;
use super::{ConflictResolution, ResolutionType};

/// Merge layers respecting policy priorities.
/// This extends the standard merge algorithm with policy-aware conflict resolution.
pub(super) fn merge_with_policy(
    layers: &[ProfileLayer],
    conflicts: &mut Vec<ConflictResolution>,
) -> std::result::Result<MergedProfile, CompositionError> {
    let mut merged = MergedProfile::default();
    // Track file ownership for conflict detection
    let mut file_owners: HashMap<std::path::PathBuf, FileOwner> = HashMap::new();

    for layer in layers {
        let spec = &layer.spec;

        // Env: later overrides earlier by name (respecting priority ordering)
        crate::merge_env(&mut merged.env, &spec.env);

        // Aliases: later overrides earlier by name
        crate::merge_aliases(&mut merged.aliases, &spec.aliases);

        // Packages: union
        if let Some(ref pkgs) = spec.packages {
            merge_packages(&mut merged.packages, pkgs);
        }

        // Files: overlay with conflict and required-resource checking
        if let Some(ref files) = spec.files {
            for managed in &files.managed {
                // Check Required-tier protection (bidirectional):
                // 1. If a Required source already owns this file, no other source can override it.
                // 2. If *this* layer is Required and another source already placed a file here, error.
                if let Some(owner) = file_owners.get(&managed.target) {
                    let cross_source = layer.source != owner.source;
                    if cross_source
                        && (owner.policy == LayerPolicy::Required
                            || layer.policy == LayerPolicy::Required)
                    {
                        return Err(CompositionError::RequiredResource {
                            source_name: if owner.policy == LayerPolicy::Required {
                                layer.source.clone()
                            } else {
                                owner.source.clone()
                            },
                            resource: managed.target.to_string_lossy().to_string(),
                        });
                    }
                    // Detect conflict between two non-local sources
                    if owner.source != "local"
                        && layer.source != "local"
                        && owner.source != layer.source
                    {
                        // Same priority = unresolvable (no deterministic winner)
                        if layer.priority == owner.priority {
                            return Err(CompositionError::UnresolvableConflict {
                                resource: managed.target.to_string_lossy().to_string(),
                                source_names: vec![owner.source.clone(), layer.source.clone()],
                            });
                        }
                        // Different priorities: higher priority wins, record override
                        conflicts.push(ConflictResolution {
                            resource_id: managed.target.to_string_lossy().to_string(),
                            resolution_type: ResolutionType::Override,
                            winning_source: layer.source.clone(),
                            details: format!(
                                "file '{}' overridden: {} (priority {}) replaces {}",
                                managed.target.posix(),
                                layer.source,
                                layer.priority,
                                owner.source
                            ),
                        });
                    }
                }

                if let Some(existing) = merged
                    .files
                    .managed
                    .iter_mut()
                    .find(|m| m.target == managed.target)
                {
                    existing.source = managed.source.clone();
                } else {
                    merged.files.managed.push(managed.clone());
                }

                file_owners.insert(
                    managed.target.clone(),
                    FileOwner {
                        source: layer.source.clone(),
                        policy: layer.policy.clone(),
                        priority: layer.priority,
                    },
                );
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

        // Secrets: append, deduplicate by source
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

        // Modules: union (deduplicated)
        union_extend(&mut merged.modules, &spec.modules);
    }

    Ok(merged)
}
