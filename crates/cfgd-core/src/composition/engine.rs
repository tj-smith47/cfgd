use std::collections::HashMap;
use std::path::PathBuf;

use crate::config::{EnvVar, ProfileLayer, ResolvedProfile, validate_secret_specs};
use crate::errors::Result;

use super::layers::build_source_layers;
use super::merge::merge_with_policy;
use super::{CompositionInput, CompositionResult, ConflictResolution};

/// Compose multiple source configs with a local resolved profile.
/// Local config is always priority 1000. Sources are merged according to policy tiers.
///
/// The composition algorithm:
/// 1. Start with local resolved profile
/// 2. For each source (sorted by priority ascending):
///    - Apply locked items unconditionally
///    - Apply required items (union for packages, source wins for files/env)
///    - Apply recommended items if accept_recommended && not rejected
///    - Apply optional items only if opted in
/// 3. Apply subscriber overrides on top
/// 4. Validate security constraints
pub fn compose(local: &ResolvedProfile, sources: &[CompositionInput]) -> Result<CompositionResult> {
    let mut all_layers: Vec<ProfileLayer> = local.layers.clone();
    let mut conflicts: Vec<ConflictResolution> = Vec::new();
    let mut source_env: HashMap<String, Vec<EnvVar>> = HashMap::new();

    // Sort sources by priority ascending (lower priority processed first, higher wins)
    let mut sorted_sources: Vec<&CompositionInput> = sources.iter().collect();
    sorted_sources.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then(a.source_name.cmp(&b.source_name))
    });

    for input in &sorted_sources {
        // Collect source-specific env for template sandboxing
        let mut env: Vec<EnvVar> = Vec::new();
        for layer in &input.layers {
            crate::merge_env(&mut env, &layer.spec.env);
        }
        source_env.insert(input.source_name.clone(), env);

        let source_layers = build_source_layers(input, &mut conflicts)?;
        all_layers.extend(source_layers);
    }

    // Sort all layers by priority, then merge
    all_layers.sort_by_key(|a| a.priority);

    let mut merged = merge_with_policy(&all_layers, &mut conflicts)?;

    // Tag files with their source origin for template sandboxing.
    // Build a HashMap<target, source> respecting layer priority order (higher priority wins).
    let mut file_origins: HashMap<PathBuf, String> = HashMap::new();
    for layer in &all_layers {
        if layer.source != "local"
            && let Some(ref files) = layer.spec.files
        {
            for managed in &files.managed {
                // Later (higher-priority) layers overwrite earlier entries
                file_origins.insert(managed.target.clone(), layer.source.clone());
            }
        }
    }
    for merged_file in &mut merged.files.managed {
        if merged_file.origin.is_none()
            && let Some(source) = file_origins.get(&merged_file.target)
        {
            merged_file.origin = Some(source.clone());
        }
    }

    // Validate secrets from all sources (catches invalid specs from ConfigSources)
    validate_secret_specs(&merged.secrets)?;

    Ok(CompositionResult {
        resolved: ResolvedProfile {
            layers: all_layers,
            merged,
        },
        conflicts,
        source_env,
        source_commits: HashMap::new(),
    })
}
