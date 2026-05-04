use crate::config::{LayerPolicy, ProfileLayer};
use crate::errors::Result;

use super::packages::filter_rejected;
use super::policy::{has_content, policy_items_to_spec};
use super::record::{record_policy_conflicts, record_rejections};
use super::{CompositionInput, ConflictResolution, ResolutionType};

/// Build profile layers from a source input, applying policy tier filtering.
pub(super) fn build_source_layers(
    input: &CompositionInput,
    conflicts: &mut Vec<ConflictResolution>,
) -> Result<Vec<ProfileLayer>> {
    let mut layers = Vec::new();
    let policy = &input.policy;

    // Locked items — unconditional, highest enforcement
    if has_content(&policy.locked) {
        let spec = policy_items_to_spec(&policy.locked);
        layers.push(ProfileLayer {
            source: input.source_name.clone(),
            profile_name: format!("{}/locked", input.source_name),
            priority: u32::MAX,            // Locked always wins
            policy: LayerPolicy::Required, // Locked uses Required policy
            spec,
        });

        record_policy_conflicts(
            &input.source_name,
            &policy.locked,
            ResolutionType::Locked,
            conflicts,
        );
    }

    // Required items — subscriber cannot override or remove
    if has_content(&policy.required) {
        let spec = policy_items_to_spec(&policy.required);
        layers.push(ProfileLayer {
            source: input.source_name.clone(),
            profile_name: format!("{}/required", input.source_name),
            priority: input.priority + 1000, // Required beats normal priority
            policy: LayerPolicy::Required,
            spec,
        });

        record_policy_conflicts(
            &input.source_name,
            &policy.required,
            ResolutionType::Required,
            conflicts,
        );
    }

    // Recommended items — applied if accepted and not rejected
    if has_content(&policy.recommended) && input.subscription.accept_recommended {
        let filtered = filter_rejected(&policy.recommended, &input.subscription.reject);
        if has_content(&filtered) {
            let spec = policy_items_to_spec(&filtered);
            layers.push(ProfileLayer {
                source: input.source_name.clone(),
                profile_name: format!("{}/recommended", input.source_name),
                priority: input.priority,
                policy: LayerPolicy::Recommended,
                spec,
            });
        }

        // Record rejections
        record_rejections(
            &input.source_name,
            &policy.recommended,
            &input.subscription.reject,
            conflicts,
        );
    }

    // Optional profiles — only if opted in
    for opt_profile in &input.subscription.opt_in {
        if policy.optional.profiles.contains(opt_profile) {
            // Find the profile in the source's layers
            for source_layer in &input.layers {
                if source_layer.profile_name == *opt_profile {
                    layers.push(ProfileLayer {
                        source: input.source_name.clone(),
                        profile_name: source_layer.profile_name.clone(),
                        priority: input.priority,
                        policy: LayerPolicy::Optional,
                        spec: source_layer.spec.clone(),
                    });
                }
            }
        }
    }

    // Standard source profile layers (non-policy)
    for source_layer in &input.layers {
        // Skip if already added via opt-in
        let already_added = layers
            .iter()
            .any(|l| l.profile_name == source_layer.profile_name);
        if !already_added {
            layers.push(ProfileLayer {
                source: input.source_name.clone(),
                profile_name: source_layer.profile_name.clone(),
                priority: input.priority,
                policy: LayerPolicy::Recommended,
                spec: source_layer.spec.clone(),
            });
        }
    }

    Ok(layers)
}

/// Track which source provided a file and its policy tier.
pub(super) struct FileOwner {
    pub(super) source: String,
    pub(super) policy: LayerPolicy,
    pub(super) priority: u32,
}
