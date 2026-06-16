use std::collections::HashMap;
use std::path::PathBuf;

use crate::config::{EnvVar, ProfileLayer, ResolvedProfile, validate_secret_specs};
use crate::errors::{CfgdError, CompositionError, Result};

use super::constraints::{check_locked_violations, validate_constraints};
use super::layers::build_source_layers;
use super::merge::merge_with_policy;
use super::packages::validate_reject_keys;
use super::policy::{has_content, policy_items_to_spec};
use super::{
    CompositionInput, CompositionResult, ConflictResolution, ConstraintMode, ConstraintViolation,
};

/// Compose multiple source configs with a local resolved profile.
/// Local config is always priority 1000. Sources are merged according to policy tiers.
///
/// The composition algorithm:
/// 1. Enforce each source's security constraints (FATAL): every profile-layer
///    spec and every non-empty policy tier (`locked`/`required`/`recommended`)
///    is checked against the source's `constraints` (no-scripts, allowed target
///    paths, system-change permission, encryption requirements); the subscriber's
///    `reject` keys are validated; and local config is checked for attempts to
///    override a source's `locked` resources. Any violation aborts composition.
/// 2. Start with the local resolved profile.
/// 3. For each source (sorted by priority ascending):
///    - Apply locked items unconditionally
///    - Apply required items (union for packages, source wins for files/env)
///    - Apply recommended items if accept_recommended && not rejected
///    - Apply optional items only if opted in
/// 4. Apply subscriber overrides on top.
/// 5. Validate secret specs from all sources.
pub fn compose(
    local: &ResolvedProfile,
    sources: &[CompositionInput],
    mode: ConstraintMode,
) -> Result<CompositionResult> {
    // Check source security constraints up front. In Enforce mode the first
    // violation aborts before any layer is built or merged (apply/plan/daemon —
    // compose() is the sole fail-closed chokepoint). In Report mode every source
    // is still visited and its violations accumulated, so a read path can surface
    // them without aborting.
    let mut constraint_violations: Vec<ConstraintViolation> = Vec::new();
    for input in sources {
        match (mode, enforce_source_constraints(input, local)) {
            (_, Ok(())) => {}
            (ConstraintMode::Enforce, Err(e)) => return Err(e),
            (ConstraintMode::Report, Err(e)) => {
                constraint_violations.push(violation_from_error(&input.source_name, e));
            }
        }
    }

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
        source_module_roots: Vec::new(),
        constraint_violations,
    })
}

/// Map a source-constraint `CfgdError` into a [`ConstraintViolation`] for Report
/// mode. The `detail` is the error's Display verbatim, so reported wording is
/// byte-identical to the fail-closed message; `kind`/`path` are extracted from
/// the underlying `CompositionError` variant. A non-`CompositionError` (which
/// `enforce_source_constraints` never produces) degrades to an `unknown` kind
/// carrying the message, so no violation is ever silently dropped.
fn violation_from_error(source_name: &str, err: CfgdError) -> ConstraintViolation {
    let detail = err.to_string();
    let (kind, path) = match &err {
        CfgdError::Composition(boxed) => match boxed.as_ref() {
            CompositionError::EncryptionRequired { path, .. } => {
                ("encryption-required", Some(path.clone()))
            }
            CompositionError::EncryptionBackendMismatch { path, .. } => {
                ("encryption-backend-mismatch", Some(path.clone()))
            }
            CompositionError::EncryptionModeMismatch { path, .. } => {
                ("encryption-mode-mismatch", Some(path.clone()))
            }
            CompositionError::PathNotAllowed { path, .. } => {
                ("path-not-allowed", Some(path.clone()))
            }
            CompositionError::ScriptsNotAllowed { .. } => ("scripts-not-allowed", None),
            CompositionError::SystemChangeNotAllowed { setting, .. } => {
                ("system-change-not-allowed", Some(setting.clone()))
            }
            CompositionError::LockedResource { resource, .. } => {
                ("locked-violation", Some(resource.clone()))
            }
            CompositionError::InvalidReject { .. } => ("reject-key-invalid", None),
            _ => ("source-constraint", None),
        },
        _ => ("source-constraint", None),
    };
    ConstraintViolation {
        source_name: source_name.to_string(),
        path,
        kind: kind.to_string(),
        detail,
    }
}

/// Run a single source's security-policy checks. Returns the FIRST violation as
/// an error (the caller decides — Enforce returns it, Report maps it to a
/// [`ConstraintViolation`] and continues to the next source). Reject-key,
/// per-layer/per-tier constraint, and locked-override checks are all covered.
fn enforce_source_constraints(input: &CompositionInput, local: &ResolvedProfile) -> Result<()> {
    // Reject-key typos would silently fail to filter, applying the unwanted item.
    validate_reject_keys(&input.source_name, &input.subscription.reject)?;

    // Every profile-layer spec the source ships must satisfy its constraints.
    for layer in &input.layers {
        validate_constraints(
            &input.source_name,
            &input.constraints,
            &layer.spec,
            input.allow_scripts,
        )?;
    }

    // Policy-tier files/scripts/system were never path/script-checked before.
    for tier in [
        &input.policy.locked,
        &input.policy.required,
        &input.policy.recommended,
    ] {
        if has_content(tier) {
            let spec = policy_items_to_spec(tier);
            validate_constraints(
                &input.source_name,
                &input.constraints,
                &spec,
                input.allow_scripts,
            )?;
        }
    }

    // Local config may not override a resource the source has locked.
    check_locked_violations(&input.source_name, &input.policy.locked, &local.merged)?;

    Ok(())
}
