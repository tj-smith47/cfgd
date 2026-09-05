use std::collections::HashMap;

use crate::PathDisplayExt;
use crate::config::{
    FilesSpec, LOCAL_LAYER, LayerPolicy, MergedProfile, ProfileLayer, ProfileSpec, ScriptSpec,
};
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
        // Destructured with no `..`: a field added to `ProfileSpec` must fail
        // to compile here (and in `config::merge_layers`) until someone says
        // what it merges to. Dropping one silently is not a theoretical risk —
        // `env_scope` was missing from this merge and every machine with a
        // subscription lost the scope it declared.
        let ProfileSpec {
            // Resolved into the layer list before composition sees it.
            inherits: _,
            modules,
            env,
            env_scope,
            aliases,
            packages,
            files,
            system,
            secrets,
            scripts,
            backups,
        } = &layer.spec;

        let layer_owner = layer.owner_token();
        // Platform-gated entries are filtered BEFORE the fold, for the same
        // reason `config::merge_layers` filters before its own: an entry that
        // does not apply here must not displace one that does.
        let platform = crate::platform::Platform::current();
        let env: Vec<crate::config::EnvVar> = crate::platform::applicable_here(env, platform)
            .cloned()
            .collect();
        let aliases: Vec<crate::config::ShellAlias> =
            crate::platform::applicable_here(aliases, platform)
                .cloned()
                .collect();
        // Env: later overrides earlier by name (respecting priority ordering);
        // `PATH` concatenates.
        crate::fold_env_layer(&mut merged.env, &env, crate::PATH_LIST_SEPARATOR);
        merged.entry_owners.claim(&layer_owner, &env, &aliases);
        for secret in secrets {
            merged.entry_owners.claim_env_names(
                &layer_owner,
                secret.envs.iter().flatten().map(String::as_str),
            );
        }

        // EnvScope: last layer that *specifies* it wins, exactly as the
        // local-only merge resolves it. Composing sources must not change how
        // far the operator's own `envScope` reaches — dropping it here left
        // every machine with a subscription on the `All` default, writing the
        // live session for a profile that asked for login files only.
        if let Some(scope) = env_scope {
            merged.env_scope = *scope;
        }

        // Aliases: later overrides earlier by name
        crate::merge_aliases(&mut merged.aliases, &aliases);

        // Packages: union
        if let Some(pkgs) = packages {
            merge_packages(&mut merged.packages, pkgs);
        }

        // Files: overlay with conflict and required-resource checking
        if let Some(files) = files {
            // Destructured for the same reason `ProfileSpec` is: the guard has
            // to reach the nested specs too, or a field added to `FilesSpec`
            // is dropped by both merges with nothing failing to compile.
            let FilesSpec {
                managed: layer_managed,
                permissions,
            } = files;
            for managed in layer_managed {
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
                    if owner.source != LOCAL_LAYER
                        && layer.source != LOCAL_LAYER
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
            for (path, mode) in permissions {
                merged.files.permissions.insert(path.clone(), mode.clone());
            }
        }

        // System: deep merge at leaf level
        for (key, value) in system {
            deep_merge_yaml(
                merged
                    .system
                    .entry(key.clone())
                    .or_insert(serde_yaml::Value::Null),
                value,
            );
        }

        // Secrets: append, deduplicate by source
        for secret in secrets {
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
        if let Some(scripts) = scripts {
            // Six hook vectors, and a seventh would otherwise be silently
            // dropped by both merges — every script a source or a parent
            // profile declared for the new hook would simply never run.
            let ScriptSpec {
                pre_apply,
                post_apply,
                pre_reconcile,
                post_reconcile,
                on_drift,
                on_change,
            } = scripts;
            merged.scripts.pre_apply.extend(pre_apply.clone());
            merged.scripts.post_apply.extend(post_apply.clone());
            merged.scripts.pre_reconcile.extend(pre_reconcile.clone());
            merged.scripts.post_reconcile.extend(post_reconcile.clone());
            merged.scripts.on_drift.extend(on_drift.clone());
            merged.scripts.on_change.extend(on_change.clone());
        }

        // Backups: append, deduplicate by name (higher-priority layer overrides)
        crate::merge_backups(&mut merged.backups, backups);

        // Modules: union (deduplicated)
        union_extend(&mut merged.modules, modules);
    }

    Ok(merged)
}
