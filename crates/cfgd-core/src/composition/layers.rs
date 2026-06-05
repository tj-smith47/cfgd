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

    // Subscriber overrides — the local consumer's per-source tweaks. Placed at
    // `priority + 1` so they beat this source's own recommended/standard items
    // (at `priority`) while staying below the source's required (`+1000`) and
    // locked (`u32::MAX`) tiers — a subscriber may refine what a source
    // recommends but never what it locked or required. Overrides ride one step
    // above the source's own items, so they share the source's rank against
    // local config (fixed 1000): below local at the default source priority
    // (500), but above local if the consumer deliberately ranks the source at
    // or above 1000 — the same "higher priority wins" rule every layer obeys.
    let has_overrides = input
        .subscription
        .overrides
        .as_mapping()
        .is_some_and(|m| !m.is_empty());
    if has_overrides {
        let spec = overrides_to_spec(&input.source_name, &input.subscription.overrides)?;
        layers.push(ProfileLayer {
            source: input.source_name.clone(),
            profile_name: format!("{}/overrides", input.source_name),
            priority: input.priority + 1,
            policy: LayerPolicy::Recommended,
            spec,
        });
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

/// Convert a subscriber's `overrides` YAML mapping into a [`ProfileSpec`].
///
/// The CLI (`cfgd source override <src> set <dotted.path> <value>`) writes env
/// and aliases in MAP form (`overrides.env.EDITOR: nvim`), matching the sibling
/// `reject` convention, whereas `ProfileSpec` deserializes them as LIST form
/// (`[{name, value}]`). The two named-map fields are pre-transformed to list
/// form before deserialization; every other field (packages/files/system/
/// modules) already matches `ProfileSpec` shape and passes through untouched.
/// `deny_unknown_fields` on `ProfileSpec` surfaces a typo'd override key as an
/// error.
fn overrides_to_spec(
    source_name: &str,
    overrides: &serde_yaml::Value,
) -> Result<crate::config::ProfileSpec> {
    let mut value = overrides.clone();
    if let Some(map) = value.as_mapping_mut() {
        named_map_to_list(map, "env", "value");
        named_map_to_list(map, "aliases", "command");
    }

    serde_yaml::from_value::<crate::config::ProfileSpec>(value).map_err(|e| {
        crate::errors::CompositionError::InvalidOverrides {
            source_name: source_name.to_string(),
            message: e.to_string(),
        }
        .into()
    })
}

/// Rewrite a named-map field (`{KEY: VAL, ...}`) into the list form
/// (`[{name: KEY, <value_field>: VAL}, ...]`) that `ProfileSpec` expects.
///
/// Only a mapping value is transformed — an absent key or an already-list value
/// is left untouched, so a hand-written list form keeps working.
fn named_map_to_list(map: &mut serde_yaml::Mapping, key: &str, value_field: &str) {
    let key_val = serde_yaml::Value::String(key.to_string());
    let Some(field) = map.get(&key_val) else {
        return;
    };
    let Some(entries) = field.as_mapping() else {
        return;
    };

    let list: Vec<serde_yaml::Value> = entries
        .iter()
        .map(|(name, value)| {
            let mut entry = serde_yaml::Mapping::new();
            entry.insert(serde_yaml::Value::String("name".to_string()), name.clone());
            entry.insert(
                serde_yaml::Value::String(value_field.to_string()),
                value.clone(),
            );
            serde_yaml::Value::Mapping(entry)
        })
        .collect();

    map.insert(key_val, serde_yaml::Value::Sequence(list));
}

/// Track which source provided a file and its policy tier.
pub(super) struct FileOwner {
    pub(super) source: String,
    pub(super) policy: LayerPolicy,
    pub(super) priority: u32,
}
