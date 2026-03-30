// Composition — multi-source merge engine with policy enforcement
// Dependency rules: depends only on config/, errors/. Must NOT import
// files/, packages/, secrets/, reconciler/, daemon/, providers/.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::config::{
    ConfigSourcePolicy, EnvVar, LayerPolicy, MergedProfile, PackagesSpec, PolicyItems,
    ProfileLayer, ProfileSpec, ResolvedProfile, SourceConstraints, SourceSpec,
    validate_secret_specs,
};
use crate::errors::{CompositionError, Result};
use crate::{deep_merge_yaml, union_extend};

/// Resolution record for conflict reporting.
#[derive(Debug, Clone)]
pub struct ConflictResolution {
    pub resource_id: String,
    pub resolution_type: ResolutionType,
    pub winning_source: String,
    pub details: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolutionType {
    Locked,
    Required,
    Override,
    Rejected,
    Default,
}

impl ResolutionType {
    pub fn label(&self) -> &str {
        match self {
            ResolutionType::Locked => "LOCKED",
            ResolutionType::Required => "REQUIRED",
            ResolutionType::Override => "OVERRIDE",
            ResolutionType::Rejected => "REJECTED",
            ResolutionType::Default => "DEFAULT",
        }
    }
}

/// Input to the composition engine: a source with its resolved profile layers and policy.
#[derive(Debug)]
pub struct CompositionInput {
    pub source_name: String,
    pub priority: u32,
    pub policy: ConfigSourcePolicy,
    pub constraints: SourceConstraints,
    pub layers: Vec<ProfileLayer>,
    pub subscription: SubscriptionConfig,
}

/// Subscription config extracted from the user's cfgd.yaml for this source.
#[derive(Debug, Clone, Default)]
pub struct SubscriptionConfig {
    pub accept_recommended: bool,
    pub opt_in: Vec<String>,
    pub overrides: serde_yaml::Value,
    pub reject: serde_yaml::Value,
}

impl SubscriptionConfig {
    pub fn from_spec(spec: &SourceSpec) -> Self {
        Self {
            accept_recommended: spec.subscription.accept_recommended,
            opt_in: spec.subscription.opt_in.clone(),
            overrides: spec.subscription.overrides.clone(),
            reject: spec.subscription.reject.clone(),
        }
    }
}

/// Result of composition: merged profile + conflict report.
#[derive(Debug)]
pub struct CompositionResult {
    pub resolved: ResolvedProfile,
    pub conflicts: Vec<ConflictResolution>,
    /// Per-source env var sets for template sandboxing.
    /// Source templates must only access their own env vars + system facts,
    /// NOT the subscriber's personal env vars.
    pub source_env: HashMap<String, Vec<EnvVar>>,
    /// Source name → commit hash, populated by the caller that has access to
    /// `SourceManager` (not by `compose()` itself, which only sees layers).
    pub source_commits: HashMap<String, String>,
}

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
    all_layers.sort_by(|a, b| a.priority.cmp(&b.priority));

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

/// Build profile layers from a source input, applying policy tier filtering.
fn build_source_layers(
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
struct FileOwner {
    source: String,
    policy: LayerPolicy,
    priority: u32,
}

/// Merge layers respecting policy priorities.
/// This extends the standard merge algorithm with policy-aware conflict resolution.
fn merge_with_policy(
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
                                managed.target.display(),
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

/// Validate security constraints for a source's contribution to the composed profile.
pub fn validate_constraints(
    source_name: &str,
    constraints: &SourceConstraints,
    spec: &ProfileSpec,
) -> Result<()> {
    // Check script constraint
    if constraints.no_scripts
        && let Some(ref scripts) = spec.scripts
        && (!scripts.pre_apply.is_empty()
            || !scripts.post_apply.is_empty()
            || !scripts.pre_reconcile.is_empty()
            || !scripts.post_reconcile.is_empty()
            || !scripts.on_drift.is_empty()
            || !scripts.on_change.is_empty())
    {
        return Err(CompositionError::ScriptsNotAllowed {
            source_name: source_name.to_string(),
        }
        .into());
    }

    // Check system change constraint
    if !constraints.allow_system_changes && !spec.system.is_empty() {
        let first_key = spec.system.keys().next().cloned().unwrap_or_default();
        return Err(CompositionError::SystemChangeNotAllowed {
            source_name: source_name.to_string(),
            setting: first_key,
        }
        .into());
    }

    // Check path containment
    if !constraints.allowed_target_paths.is_empty()
        && let Some(ref files) = spec.files
    {
        for managed in &files.managed {
            let target_str = managed.target.to_string_lossy();
            if !path_matches_any(&target_str, &constraints.allowed_target_paths) {
                return Err(CompositionError::PathNotAllowed {
                    source_name: source_name.to_string(),
                    path: target_str.to_string(),
                }
                .into());
            }
        }
    }

    // Check encryption.requiredTargets: every file whose target matches a required-encryption
    // glob must have an encryption block, and if the constraint specifies a backend, it must
    // match the file's encryption backend.
    if let Some(ref enc_constraint) = constraints.encryption
        && !enc_constraint.required_targets.is_empty()
        && let Some(ref files) = spec.files
    {
        for managed in &files.managed {
            let target_str = managed.target.to_string_lossy();
            if let Some(matched_pattern) =
                find_matching_pattern(&target_str, &enc_constraint.required_targets)
            {
                match managed.encryption.as_ref() {
                    None => {
                        return Err(CompositionError::EncryptionRequired {
                            source_name: source_name.to_string(),
                            path: target_str.to_string(),
                            pattern: matched_pattern,
                        }
                        .into());
                    }
                    Some(enc_spec) => {
                        if let Some(ref required_backend) = enc_constraint.backend
                            && enc_spec.backend != *required_backend
                        {
                            return Err(CompositionError::EncryptionBackendMismatch {
                                source_name: source_name.to_string(),
                                path: target_str.to_string(),
                                pattern: matched_pattern.clone(),
                                actual_backend: enc_spec.backend.clone(),
                                required_backend: required_backend.clone(),
                            }
                            .into());
                        }
                        if let Some(ref required_mode) = enc_constraint.mode
                            && enc_spec.mode != *required_mode
                        {
                            return Err(CompositionError::EncryptionModeMismatch {
                                source_name: source_name.to_string(),
                                path: target_str.to_string(),
                                pattern: matched_pattern,
                                actual_mode: format!("{:?}", enc_spec.mode),
                                required_mode: format!("{:?}", required_mode),
                            }
                            .into());
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

/// Check if a path matches any of the allowed patterns.
/// Supports glob patterns and prefix matching.
fn path_matches_any(path: &str, allowed: &[String]) -> bool {
    find_matching_pattern(path, allowed).is_some()
}

/// Return the first pattern from `patterns` that matches `path`, or `None`.
/// Uses the same matching logic as `path_matches_any`.
fn find_matching_pattern(path: &str, patterns: &[String]) -> Option<String> {
    for pattern in patterns {
        if let Ok(glob_pattern) = glob::Pattern::new(pattern)
            && glob_pattern.matches(path)
        {
            return Some(pattern.clone());
        }
        if pattern.ends_with('/') && path.starts_with(pattern.as_str()) {
            return Some(pattern.clone());
        }
        if path == pattern {
            return Some(pattern.clone());
        }
    }
    None
}

/// Check if a subscriber is trying to override a locked resource.
pub fn check_locked_violations(
    source_name: &str,
    locked: &PolicyItems,
    local_merged: &MergedProfile,
) -> Result<()> {
    // Check locked files — local cannot override these targets
    for locked_file in &locked.files {
        for local_file in &local_merged.files.managed {
            if local_file.target == locked_file.target && local_file.source != locked_file.source {
                return Err(CompositionError::LockedResource {
                    source_name: source_name.to_string(),
                    resource: locked_file.target.to_string_lossy().to_string(),
                }
                .into());
            }
        }
    }

    Ok(())
}

// --- Helper functions ---

fn has_content(items: &PolicyItems) -> bool {
    items.packages.is_some()
        || !items.files.is_empty()
        || !items.env.is_empty()
        || !items.aliases.is_empty()
        || !items.system.is_empty()
        || !items.profiles.is_empty()
        || !items.modules.is_empty()
        || !items.secrets.is_empty()
}

fn policy_items_to_spec(items: &PolicyItems) -> ProfileSpec {
    ProfileSpec {
        packages: items.packages.clone(),
        files: if items.files.is_empty() {
            None
        } else {
            Some(crate::config::FilesSpec {
                managed: items.files.clone(),
                permissions: HashMap::new(),
            })
        },
        env: items.env.clone(),
        aliases: items.aliases.clone(),
        system: items.system.clone(),
        modules: items.modules.clone(),
        secrets: items.secrets.clone(),
        ..Default::default()
    }
}

/// Merge packages from `source` into `target`, unioning lists and applying
/// later-wins for scalar fields (file paths, remotes, custom manager commands).
pub fn merge_packages(target: &mut PackagesSpec, source: &PackagesSpec) {
    if let Some(ref brew) = source.brew {
        let target_brew = target.brew.get_or_insert_with(Default::default);
        if brew.file.is_some() {
            target_brew.file = brew.file.clone();
        }
        union_extend(&mut target_brew.taps, &brew.taps);
        union_extend(&mut target_brew.formulae, &brew.formulae);
        union_extend(&mut target_brew.casks, &brew.casks);
    }
    if let Some(ref apt) = source.apt {
        let target_apt = target.apt.get_or_insert_with(Default::default);
        if apt.file.is_some() {
            target_apt.file = apt.file.clone();
        }
        union_extend(&mut target_apt.packages, &apt.packages);
    }
    if let Some(ref cargo) = source.cargo {
        let target_cargo = target.cargo.get_or_insert_with(Default::default);
        if cargo.file.is_some() {
            target_cargo.file = cargo.file.clone();
        }
        union_extend(&mut target_cargo.packages, &cargo.packages);
    }
    if let Some(ref npm) = source.npm {
        let target_npm = target.npm.get_or_insert_with(Default::default);
        if npm.file.is_some() {
            target_npm.file = npm.file.clone();
        }
        union_extend(&mut target_npm.global, &npm.global);
    }
    union_extend(&mut target.pipx, &source.pipx);
    union_extend(&mut target.dnf, &source.dnf);
    union_extend(&mut target.apk, &source.apk);
    union_extend(&mut target.pacman, &source.pacman);
    union_extend(&mut target.zypper, &source.zypper);
    union_extend(&mut target.yum, &source.yum);
    union_extend(&mut target.pkg, &source.pkg);
    if let Some(ref snap) = source.snap {
        let target_snap = target.snap.get_or_insert_with(Default::default);
        union_extend(&mut target_snap.packages, &snap.packages);
        union_extend(&mut target_snap.classic, &snap.classic);
    }
    if let Some(ref flatpak) = source.flatpak {
        let target_flatpak = target.flatpak.get_or_insert_with(Default::default);
        union_extend(&mut target_flatpak.packages, &flatpak.packages);
        if flatpak.remote.is_some() {
            target_flatpak.remote = flatpak.remote.clone();
        }
    }
    union_extend(&mut target.nix, &source.nix);
    union_extend(&mut target.go, &source.go);
    union_extend(&mut target.winget, &source.winget);
    union_extend(&mut target.chocolatey, &source.chocolatey);
    union_extend(&mut target.scoop, &source.scoop);
    // Custom managers: merge by name, union packages
    for custom in &source.custom {
        if let Some(existing) = target.custom.iter_mut().find(|c| c.name == custom.name) {
            existing.check = custom.check.clone();
            existing.list_installed = custom.list_installed.clone();
            existing.install = custom.install.clone();
            existing.uninstall = custom.uninstall.clone();
            if custom.update.is_some() {
                existing.update = custom.update.clone();
            }
            union_extend(&mut existing.packages, &custom.packages);
        } else {
            target.custom.push(custom.clone());
        }
    }
}

/// Filter rejected items from recommended policy items.
fn filter_rejected(recommended: &PolicyItems, reject: &serde_yaml::Value) -> PolicyItems {
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

fn record_policy_conflicts(
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

fn record_rejections(
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

fn collect_package_names(pkgs: &PackagesSpec) -> Vec<String> {
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

fn count_policy_tier_items(items: &PolicyItems) -> usize {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::*;

    fn make_local_profile() -> ResolvedProfile {
        ResolvedProfile {
            layers: vec![ProfileLayer {
                source: "local".into(),
                profile_name: "default".into(),
                priority: 1000,
                policy: LayerPolicy::Local,
                spec: ProfileSpec {
                    env: vec![EnvVar {
                        name: "editor".into(),
                        value: "vim".into(),
                    }],
                    packages: Some(PackagesSpec {
                        cargo: Some(CargoSpec {
                            file: None,
                            packages: vec!["bat".into()],
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            }],
            merged: MergedProfile {
                env: vec![EnvVar {
                    name: "editor".into(),
                    value: "vim".into(),
                }],
                packages: PackagesSpec {
                    cargo: Some(CargoSpec {
                        file: None,
                        packages: vec!["bat".into()],
                    }),
                    ..Default::default()
                },
                ..Default::default()
            },
        }
    }

    fn make_source_input(name: &str, priority: u32) -> CompositionInput {
        CompositionInput {
            source_name: name.into(),
            priority,
            policy: ConfigSourcePolicy {
                required: PolicyItems {
                    packages: Some(PackagesSpec {
                        brew: Some(BrewSpec {
                            formulae: vec!["git-secrets".into()],
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                recommended: PolicyItems {
                    packages: Some(PackagesSpec {
                        brew: Some(BrewSpec {
                            formulae: vec!["k9s".into(), "stern".into()],
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    env: vec![EnvVar {
                        name: "EDITOR".into(),
                        value: "code --wait".into(),
                    }],
                    ..Default::default()
                },
                locked: PolicyItems {
                    files: vec![ManagedFileSpec {
                        source: "security/policy.yaml".into(),
                        target: "~/.config/company/security-policy.yaml".into(),
                        strategy: None,
                        private: false,
                        origin: None,
                        encryption: None,
                        permissions: None,
                    }],
                    ..Default::default()
                },
                ..Default::default()
            },
            constraints: SourceConstraints::default(),
            layers: vec![],
            subscription: SubscriptionConfig {
                accept_recommended: true,
                ..Default::default()
            },
        }
    }

    #[test]
    fn compose_with_no_sources() {
        let local = make_local_profile();
        let result = compose(&local, &[]).unwrap();
        assert_eq!(
            result
                .resolved
                .merged
                .env
                .iter()
                .find(|e| e.name == "editor")
                .map(|e| &e.value),
            Some(&"vim".to_string())
        );
        assert!(result.conflicts.is_empty());
    }

    #[test]
    fn compose_applies_required_packages() {
        let local = make_local_profile();
        let input = make_source_input("acme", 500);
        let result = compose(&local, &[input]).unwrap();

        let brew = result.resolved.merged.packages.brew.as_ref().unwrap();
        assert!(brew.formulae.contains(&"git-secrets".into()));
    }

    #[test]
    fn compose_applies_recommended_when_accepted() {
        let local = make_local_profile();
        let input = make_source_input("acme", 500);
        let result = compose(&local, &[input]).unwrap();

        let brew = result.resolved.merged.packages.brew.as_ref().unwrap();
        assert!(brew.formulae.contains(&"k9s".into()));
        assert!(brew.formulae.contains(&"stern".into()));
    }

    #[test]
    fn compose_skips_recommended_when_not_accepted() {
        let local = make_local_profile();
        let mut input = make_source_input("acme", 500);
        input.subscription.accept_recommended = false;
        let result = compose(&local, &[input]).unwrap();

        // k9s should NOT be present (recommended not accepted)
        let has_k9s = result
            .resolved
            .merged
            .packages
            .brew
            .as_ref()
            .map(|b| b.formulae.contains(&"k9s".into()))
            .unwrap_or(false);
        assert!(!has_k9s);
    }

    #[test]
    fn compose_rejects_recommended_packages() {
        let local = make_local_profile();
        let mut input = make_source_input("acme", 500);

        // Reject kubectx from recommended
        let reject_yaml: serde_yaml::Value = serde_yaml::from_str(
            r#"
            packages:
              brew:
                formulae:
                  - stern
            "#,
        )
        .unwrap();
        input.subscription.reject = reject_yaml;

        let result = compose(&local, &[input]).unwrap();
        let brew = result.resolved.merged.packages.brew.as_ref().unwrap();
        assert!(brew.formulae.contains(&"k9s".into()));
        assert!(!brew.formulae.contains(&"stern".into()));
    }

    #[test]
    fn compose_records_locked_conflicts() {
        let local = make_local_profile();
        let input = make_source_input("acme", 500);
        let result = compose(&local, &[input]).unwrap();

        let locked_conflicts: Vec<_> = result
            .conflicts
            .iter()
            .filter(|c| c.resolution_type == ResolutionType::Locked)
            .collect();
        assert!(!locked_conflicts.is_empty());
    }

    #[test]
    fn compose_is_deterministic() {
        let local = make_local_profile();
        let input1 = make_source_input("acme", 500);
        let input2 = make_source_input("acme", 500);

        let result1 = compose(&local, &[input1]).unwrap();
        let result2 = compose(&local, &[input2]).unwrap();

        // Same packages in same order
        assert_eq!(
            result1.resolved.merged.packages.cargo,
            result2.resolved.merged.packages.cargo
        );
    }

    #[test]
    fn validate_constraints_scripts_blocked() {
        let constraints = SourceConstraints {
            no_scripts: true,
            ..Default::default()
        };
        let spec = ProfileSpec {
            scripts: Some(ScriptSpec {
                pre_reconcile: vec![ScriptEntry::Simple("setup.sh".to_string())],
                ..Default::default()
            }),
            ..Default::default()
        };
        let result = validate_constraints("acme", &constraints, &spec);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("scripts"));
    }

    #[test]
    fn validate_constraints_scripts_allowed() {
        let constraints = SourceConstraints {
            no_scripts: false,
            ..Default::default()
        };
        let spec = ProfileSpec {
            scripts: Some(ScriptSpec {
                pre_reconcile: vec![ScriptEntry::Simple("setup.sh".to_string())],
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(validate_constraints("acme", &constraints, &spec).is_ok());
    }

    #[test]
    fn validate_constraints_path_containment() {
        let constraints = SourceConstraints {
            allowed_target_paths: vec!["~/.config/acme/".into()],
            ..Default::default()
        };
        let spec = ProfileSpec {
            files: Some(FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "evil.sh".into(),
                    target: "/etc/sudoers".into(),
                    strategy: None,
                    private: false,
                    origin: None,
                    encryption: None,
                    permissions: None,
                }],
                ..Default::default()
            }),
            ..Default::default()
        };
        let result = validate_constraints("acme", &constraints, &spec);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("allowed paths"));
    }

    #[test]
    fn validate_constraints_path_allowed() {
        let constraints = SourceConstraints {
            allowed_target_paths: vec!["~/.config/acme/*".into()],
            ..Default::default()
        };
        let spec = ProfileSpec {
            files: Some(FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "config.yaml".into(),
                    target: "~/.config/acme/config.yaml".into(),
                    strategy: None,
                    private: false,
                    origin: None,
                    encryption: None,
                    permissions: None,
                }],
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(validate_constraints("acme", &constraints, &spec).is_ok());
    }

    #[test]
    fn validate_constraints_system_changes_blocked() {
        let constraints = SourceConstraints {
            allow_system_changes: false,
            ..Default::default()
        };
        let spec = ProfileSpec {
            system: HashMap::from([("shell".into(), serde_yaml::Value::String("/bin/zsh".into()))]),
            ..Default::default()
        };
        let result = validate_constraints("acme", &constraints, &spec);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("system"));
    }

    #[test]
    fn path_matches_glob_pattern() {
        assert!(path_matches_any(
            "~/.config/acme/config.yaml",
            &["~/.config/acme/*".into()]
        ));
        assert!(!path_matches_any(
            "/etc/sudoers",
            &["~/.config/acme/*".into()]
        ));
    }

    #[test]
    fn path_matches_prefix() {
        assert!(path_matches_any(
            "~/.config/acme/deep/file.yaml",
            &["~/.config/acme/".into()]
        ));
    }

    #[test]
    fn path_matches_exact() {
        assert!(path_matches_any(
            "~/.eslintrc.json",
            &["~/.eslintrc.json".into()]
        ));
    }

    #[test]
    fn filter_rejected_removes_packages() {
        let recommended = PolicyItems {
            packages: Some(PackagesSpec {
                brew: Some(BrewSpec {
                    formulae: vec!["k9s".into(), "stern".into(), "kubectx".into()],
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };

        let reject: serde_yaml::Value = serde_yaml::from_str(
            r#"
            packages:
              brew:
                formulae:
                  - kubectx
            "#,
        )
        .unwrap();

        let filtered = filter_rejected(&recommended, &reject);
        let brew = filtered.packages.unwrap().brew.unwrap();
        assert!(brew.formulae.contains(&"k9s".into()));
        assert!(brew.formulae.contains(&"stern".into()));
        assert!(!brew.formulae.contains(&"kubectx".into()));
    }

    #[test]
    fn filter_rejected_noop_on_null() {
        let recommended = PolicyItems {
            env: vec![EnvVar {
                name: "EDITOR".into(),
                value: "code".into(),
            }],
            ..Default::default()
        };

        let filtered = filter_rejected(&recommended, &serde_yaml::Value::Null);
        assert_eq!(filtered.env.len(), 1);
    }

    #[test]
    fn multiple_sources_priority_ordering() {
        let local = make_local_profile();
        let source_a = CompositionInput {
            source_name: "alpha".into(),
            priority: 300,
            policy: ConfigSourcePolicy {
                recommended: PolicyItems {
                    env: vec![EnvVar {
                        name: "theme".into(),
                        value: "dark".into(),
                    }],
                    ..Default::default()
                },
                ..Default::default()
            },
            constraints: Default::default(),
            layers: vec![],
            subscription: SubscriptionConfig {
                accept_recommended: true,
                ..Default::default()
            },
        };
        let source_b = CompositionInput {
            source_name: "beta".into(),
            priority: 700,
            policy: ConfigSourcePolicy {
                recommended: PolicyItems {
                    env: vec![EnvVar {
                        name: "theme".into(),
                        value: "light".into(),
                    }],
                    ..Default::default()
                },
                ..Default::default()
            },
            constraints: Default::default(),
            layers: vec![],
            subscription: SubscriptionConfig {
                accept_recommended: true,
                ..Default::default()
            },
        };

        let result = compose(&local, &[source_a, source_b]).unwrap();
        // Local (1000) wins over both sources, so "editor" = "vim" still
        assert_eq!(
            result
                .resolved
                .merged
                .env
                .iter()
                .find(|e| e.name == "editor")
                .map(|e| &e.value),
            Some(&"vim".to_string())
        );
        // Between sources, local wins (priority 1000), but for "theme" which is only
        // in sources, higher priority (beta=700) processed after lower priority (alpha=300)
        // Local layers are priority 1000 so processed last, but "theme" isn't in local
        // so beta's value should remain
        assert_eq!(
            result
                .resolved
                .merged
                .env
                .iter()
                .find(|e| e.name == "theme")
                .map(|e| &e.value),
            Some(&"light".to_string())
        );
    }

    #[test]
    fn required_resource_cannot_be_overridden() {
        let local = make_local_profile();
        let source = CompositionInput {
            source_name: "corp".into(),
            priority: 500,
            policy: ConfigSourcePolicy {
                required: PolicyItems {
                    files: vec![ManagedFileSpec {
                        source: "corp/policy.yaml".into(),
                        target: "~/.config/policy.yaml".into(),
                        strategy: None,
                        private: false,
                        origin: None,
                        encryption: None,
                        permissions: None,
                    }],
                    ..Default::default()
                },
                ..Default::default()
            },
            constraints: Default::default(),
            layers: vec![],
            subscription: SubscriptionConfig {
                accept_recommended: true,
                ..Default::default()
            },
        };
        // Second source tries to override the same file
        let source_b = CompositionInput {
            source_name: "rogue".into(),
            priority: 600,
            policy: ConfigSourcePolicy {
                recommended: PolicyItems {
                    files: vec![ManagedFileSpec {
                        source: "rogue/policy.yaml".into(),
                        target: "~/.config/policy.yaml".into(),
                        strategy: None,
                        private: false,
                        origin: None,
                        encryption: None,
                        permissions: None,
                    }],
                    ..Default::default()
                },
                ..Default::default()
            },
            constraints: Default::default(),
            layers: vec![],
            subscription: SubscriptionConfig {
                accept_recommended: true,
                ..Default::default()
            },
        };

        let result = compose(&local, &[source, source_b]);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("required resource"));
        assert!(err.contains("policy.yaml"));
    }

    #[test]
    fn file_conflict_between_sources_records_resolution() {
        let local = make_local_profile();
        let source_a = CompositionInput {
            source_name: "alpha".into(),
            priority: 300,
            policy: ConfigSourcePolicy {
                recommended: PolicyItems {
                    files: vec![ManagedFileSpec {
                        source: "alpha/tool.conf".into(),
                        target: "~/.config/tool.conf".into(),
                        strategy: None,
                        private: false,
                        origin: None,
                        encryption: None,
                        permissions: None,
                    }],
                    ..Default::default()
                },
                ..Default::default()
            },
            constraints: Default::default(),
            layers: vec![],
            subscription: SubscriptionConfig {
                accept_recommended: true,
                ..Default::default()
            },
        };
        let source_b = CompositionInput {
            source_name: "beta".into(),
            priority: 700,
            policy: ConfigSourcePolicy {
                recommended: PolicyItems {
                    files: vec![ManagedFileSpec {
                        source: "beta/tool.conf".into(),
                        target: "~/.config/tool.conf".into(),
                        strategy: None,
                        private: false,
                        origin: None,
                        encryption: None,
                        permissions: None,
                    }],
                    ..Default::default()
                },
                ..Default::default()
            },
            constraints: Default::default(),
            layers: vec![],
            subscription: SubscriptionConfig {
                accept_recommended: true,
                ..Default::default()
            },
        };

        let result = compose(&local, &[source_a, source_b]).unwrap();
        // Beta wins (higher priority)
        let file = result
            .resolved
            .merged
            .files
            .managed
            .iter()
            .find(|f| f.target.to_string_lossy().contains("tool.conf"))
            .unwrap();
        assert_eq!(file.source, "beta/tool.conf");
        // Conflict recorded
        let conflict = result
            .conflicts
            .iter()
            .find(|c| c.resource_id.contains("tool.conf"));
        assert!(conflict.is_some());
        assert_eq!(conflict.unwrap().winning_source, "beta");
    }

    #[test]
    fn equal_priority_file_conflict_is_unresolvable() {
        let local = make_local_profile();
        let source_a = CompositionInput {
            source_name: "team-a".into(),
            priority: 500,
            policy: ConfigSourcePolicy {
                recommended: PolicyItems {
                    files: vec![ManagedFileSpec {
                        source: "team-a/settings.json".into(),
                        target: "~/.config/settings.json".into(),
                        strategy: None,
                        private: false,
                        origin: None,
                        encryption: None,
                        permissions: None,
                    }],
                    ..Default::default()
                },
                ..Default::default()
            },
            constraints: Default::default(),
            layers: vec![],
            subscription: SubscriptionConfig {
                accept_recommended: true,
                ..Default::default()
            },
        };
        let source_b = CompositionInput {
            source_name: "team-b".into(),
            priority: 500,
            policy: ConfigSourcePolicy {
                recommended: PolicyItems {
                    files: vec![ManagedFileSpec {
                        source: "team-b/settings.json".into(),
                        target: "~/.config/settings.json".into(),
                        strategy: None,
                        private: false,
                        origin: None,
                        encryption: None,
                        permissions: None,
                    }],
                    ..Default::default()
                },
                ..Default::default()
            },
            constraints: Default::default(),
            layers: vec![],
            subscription: SubscriptionConfig {
                accept_recommended: true,
                ..Default::default()
            },
        };

        let result = compose(&local, &[source_a, source_b]);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("conflict"));
        assert!(err.contains("settings.json"));
    }

    #[test]
    fn required_modules_always_included() {
        let local = make_local_profile();
        let mut source = make_source_input("acme", 500);
        source.policy.required.modules = vec!["corp-vpn".into(), "corp-certs".into()];
        let result = compose(&local, &[source]).unwrap();
        assert!(
            result
                .resolved
                .merged
                .modules
                .contains(&"corp-vpn".to_string())
        );
        assert!(
            result
                .resolved
                .merged
                .modules
                .contains(&"corp-certs".to_string())
        );
    }

    #[test]
    fn recommended_modules_included_when_accepted() {
        let local = make_local_profile();
        let mut source = make_source_input("acme", 500);
        source.policy.recommended.modules = vec!["editor".into()];
        source.subscription.accept_recommended = true;
        let result = compose(&local, &[source]).unwrap();
        assert!(
            result
                .resolved
                .merged
                .modules
                .contains(&"editor".to_string())
        );
    }

    #[test]
    fn recommended_modules_rejected() {
        let local = make_local_profile();
        let mut source = make_source_input("acme", 500);
        source.policy.recommended.modules = vec!["editor".into()];
        source.subscription.accept_recommended = true;
        source.subscription.reject = serde_yaml::from_str("modules: [editor]").unwrap();
        let result = compose(&local, &[source]).unwrap();
        assert!(
            !result
                .resolved
                .merged
                .modules
                .contains(&"editor".to_string())
        );
        // Verify rejection is recorded in conflicts
        assert!(
            result
                .conflicts
                .iter()
                .any(|c| c.resource_id == "module:editor"
                    && c.resolution_type == ResolutionType::Rejected)
        );
    }

    #[test]
    fn module_policy_conflicts_recorded() {
        let local = make_local_profile();
        let mut source = make_source_input("acme", 500);
        source.policy.required.modules = vec!["corp-vpn".into()];
        let result = compose(&local, &[source]).unwrap();
        assert!(
            result
                .conflicts
                .iter()
                .any(|c| c.resource_id == "module:corp-vpn"
                    && c.resolution_type == ResolutionType::Required)
        );
    }

    #[test]
    fn local_modules_and_source_modules_union() {
        let mut local = make_local_profile();
        local.layers[0].spec.modules = vec!["nvim".into()];
        local.merged.modules = vec!["nvim".into()];
        let mut source = make_source_input("acme", 500);
        source.policy.required.modules = vec!["corp-vpn".into()];
        let result = compose(&local, &[source]).unwrap();
        assert!(result.resolved.merged.modules.contains(&"nvim".to_string()));
        assert!(
            result
                .resolved
                .merged
                .modules
                .contains(&"corp-vpn".to_string())
        );
    }

    #[test]
    fn count_policy_tier_items_includes_modules() {
        let items = PolicyItems {
            modules: vec!["a".into(), "b".into()],
            ..Default::default()
        };
        assert_eq!(count_policy_tier_items(&items), 2);
    }

    // --- Encryption constraint tests ---

    fn make_encryption_constraint(patterns: &[&str], backend: Option<&str>) -> SourceConstraints {
        SourceConstraints {
            encryption: Some(crate::config::EncryptionConstraint {
                required_targets: patterns.iter().map(|s| s.to_string()).collect(),
                backend: backend.map(|s| s.to_string()),
                mode: None,
            }),
            ..Default::default()
        }
    }

    fn make_file_spec_with_encryption(target: &str, backend: Option<&str>) -> ProfileSpec {
        ProfileSpec {
            files: Some(FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "source/file".into(),
                    target: target.into(),
                    strategy: None,
                    private: false,
                    origin: None,
                    encryption: backend.map(|b| crate::config::EncryptionSpec {
                        backend: b.to_string(),
                        mode: crate::config::EncryptionMode::InRepo,
                    }),
                    permissions: None,
                }],
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn encryption_required_target_without_encryption_is_error() {
        let constraints = make_encryption_constraint(&["~/.ssh/*"], None);
        let spec = make_file_spec_with_encryption("~/.ssh/id_rsa", None);
        let result = validate_constraints("corp", &constraints, &spec);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("~/.ssh/id_rsa"), "msg: {msg}");
        assert!(msg.contains("~/.ssh/*"), "msg: {msg}");
        assert!(msg.contains("encryption"), "msg: {msg}");
    }

    #[test]
    fn encryption_required_target_with_encryption_passes() {
        let constraints = make_encryption_constraint(&["~/.ssh/*"], None);
        let spec = make_file_spec_with_encryption("~/.ssh/id_rsa", Some("sops"));
        assert!(validate_constraints("corp", &constraints, &spec).is_ok());
    }

    #[test]
    fn encryption_non_matching_target_without_encryption_passes() {
        let constraints = make_encryption_constraint(&["~/.ssh/*"], None);
        // ~/.zshrc does not match ~/.ssh/* — no enforcement
        let spec = make_file_spec_with_encryption("~/.zshrc", None);
        assert!(validate_constraints("corp", &constraints, &spec).is_ok());
    }

    #[test]
    fn encryption_empty_required_targets_no_enforcement() {
        let constraints = SourceConstraints {
            encryption: Some(crate::config::EncryptionConstraint {
                required_targets: vec![],
                backend: Some("sops".into()),
                mode: None,
            }),
            ..Default::default()
        };
        // Even though a backend is specified, empty requiredTargets means no enforcement
        let spec = make_file_spec_with_encryption("~/.ssh/id_rsa", None);
        assert!(validate_constraints("corp", &constraints, &spec).is_ok());
    }

    #[test]
    fn encryption_no_constraint_field_no_enforcement() {
        let constraints = SourceConstraints::default();
        // No encryption constraint at all
        let spec = make_file_spec_with_encryption("~/.ssh/id_rsa", None);
        assert!(validate_constraints("corp", &constraints, &spec).is_ok());
    }

    #[test]
    fn encryption_wrong_backend_is_error() {
        let constraints = make_encryption_constraint(&["~/.aws/*"], Some("sops"));
        let spec = make_file_spec_with_encryption("~/.aws/credentials", Some("age"));
        let result = validate_constraints("corp", &constraints, &spec);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("~/.aws/credentials"), "msg: {msg}");
        assert!(msg.contains("age"), "msg: {msg}");
        assert!(msg.contains("sops"), "msg: {msg}");
    }

    #[test]
    fn encryption_correct_backend_passes() {
        let constraints = make_encryption_constraint(&["~/.aws/*"], Some("sops"));
        let spec = make_file_spec_with_encryption("~/.aws/credentials", Some("sops"));
        assert!(validate_constraints("corp", &constraints, &spec).is_ok());
    }

    #[test]
    fn encryption_constraint_matches_exact_path() {
        let constraints = make_encryption_constraint(&["~/.gnupg/secring.gpg"], None);
        // Exact path match
        let spec = make_file_spec_with_encryption("~/.gnupg/secring.gpg", None);
        let result = validate_constraints("corp", &constraints, &spec);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("~/.gnupg/secring.gpg")
        );
    }

    // --- Determinism: full merged output comparison ---

    #[test]
    fn compose_is_deterministic_full_output() {
        let local = make_local_profile();
        let input1 = make_source_input("acme", 500);
        let input2 = make_source_input("acme", 500);

        let result1 = compose(&local, &[input1]).unwrap();
        let result2 = compose(&local, &[input2]).unwrap();

        // Serialize both merged profiles and compare the full output
        let yaml1 = serde_yaml::to_string(&result1.resolved.merged).unwrap();
        let yaml2 = serde_yaml::to_string(&result2.resolved.merged).unwrap();
        assert_eq!(yaml1, yaml2, "Full merged output must be identical across runs");

        // Also check conflict counts and types match
        assert_eq!(result1.conflicts.len(), result2.conflicts.len());
        for (c1, c2) in result1.conflicts.iter().zip(result2.conflicts.iter()) {
            assert_eq!(c1.resource_id, c2.resource_id);
            assert_eq!(c1.resolution_type, c2.resolution_type);
            assert_eq!(c1.winning_source, c2.winning_source);
        }
    }

    #[test]
    fn compose_deterministic_with_multiple_sources() {
        let local = make_local_profile();
        // Run twice with same sources in same order
        let mk = || {
            vec![
                CompositionInput {
                    source_name: "alpha".into(),
                    priority: 300,
                    policy: ConfigSourcePolicy {
                        recommended: PolicyItems {
                            packages: Some(PackagesSpec {
                                brew: Some(BrewSpec {
                                    formulae: vec!["ripgrep".into(), "fd".into()],
                                    ..Default::default()
                                }),
                                ..Default::default()
                            }),
                            env: vec![EnvVar {
                                name: "PAGER".into(),
                                value: "less".into(),
                            }],
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                    constraints: Default::default(),
                    layers: vec![],
                    subscription: SubscriptionConfig {
                        accept_recommended: true,
                        ..Default::default()
                    },
                },
                CompositionInput {
                    source_name: "beta".into(),
                    priority: 700,
                    policy: ConfigSourcePolicy {
                        recommended: PolicyItems {
                            packages: Some(PackagesSpec {
                                brew: Some(BrewSpec {
                                    formulae: vec!["jq".into()],
                                    ..Default::default()
                                }),
                                ..Default::default()
                            }),
                            env: vec![EnvVar {
                                name: "PAGER".into(),
                                value: "bat".into(),
                            }],
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                    constraints: Default::default(),
                    layers: vec![],
                    subscription: SubscriptionConfig {
                        accept_recommended: true,
                        ..Default::default()
                    },
                },
            ]
        };

        let r1 = compose(&local, &mk()).unwrap();
        let r2 = compose(&local, &mk()).unwrap();
        let yaml1 = serde_yaml::to_string(&r1.resolved.merged).unwrap();
        let yaml2 = serde_yaml::to_string(&r2.resolved.merged).unwrap();
        assert_eq!(yaml1, yaml2);
    }

    // --- Conflict resolution: env var priority ---

    #[test]
    fn higher_priority_source_wins_env_var() {
        let local = ResolvedProfile {
            layers: vec![ProfileLayer {
                source: "local".into(),
                profile_name: "default".into(),
                priority: 1000,
                policy: LayerPolicy::Local,
                spec: ProfileSpec::default(),
            }],
            merged: MergedProfile::default(),
        };
        let low = CompositionInput {
            source_name: "low".into(),
            priority: 200,
            policy: ConfigSourcePolicy {
                recommended: PolicyItems {
                    env: vec![EnvVar {
                        name: "THEME".into(),
                        value: "solarized".into(),
                    }],
                    ..Default::default()
                },
                ..Default::default()
            },
            constraints: Default::default(),
            layers: vec![],
            subscription: SubscriptionConfig {
                accept_recommended: true,
                ..Default::default()
            },
        };
        let high = CompositionInput {
            source_name: "high".into(),
            priority: 800,
            policy: ConfigSourcePolicy {
                recommended: PolicyItems {
                    env: vec![EnvVar {
                        name: "THEME".into(),
                        value: "dracula".into(),
                    }],
                    ..Default::default()
                },
                ..Default::default()
            },
            constraints: Default::default(),
            layers: vec![],
            subscription: SubscriptionConfig {
                accept_recommended: true,
                ..Default::default()
            },
        };

        let result = compose(&local, &[low, high]).unwrap();
        let theme = result
            .resolved
            .merged
            .env
            .iter()
            .find(|e| e.name == "THEME")
            .expect("THEME env var must exist");
        // Higher priority (800) source processed after lower (200), so it overwrites
        assert_eq!(theme.value, "dracula");
    }

    #[test]
    fn local_env_wins_over_source_env_at_same_name() {
        let local = ResolvedProfile {
            layers: vec![ProfileLayer {
                source: "local".into(),
                profile_name: "default".into(),
                priority: 1000,
                policy: LayerPolicy::Local,
                spec: ProfileSpec {
                    env: vec![EnvVar {
                        name: "EDITOR".into(),
                        value: "nvim".into(),
                    }],
                    ..Default::default()
                },
            }],
            merged: MergedProfile {
                env: vec![EnvVar {
                    name: "EDITOR".into(),
                    value: "nvim".into(),
                }],
                ..Default::default()
            },
        };
        let source = CompositionInput {
            source_name: "corp".into(),
            priority: 500,
            policy: ConfigSourcePolicy {
                recommended: PolicyItems {
                    env: vec![EnvVar {
                        name: "EDITOR".into(),
                        value: "code --wait".into(),
                    }],
                    ..Default::default()
                },
                ..Default::default()
            },
            constraints: Default::default(),
            layers: vec![],
            subscription: SubscriptionConfig {
                accept_recommended: true,
                ..Default::default()
            },
        };

        let result = compose(&local, &[source]).unwrap();
        let editor = result
            .resolved
            .merged
            .env
            .iter()
            .find(|e| e.name == "EDITOR")
            .expect("EDITOR env var must exist");
        // Local priority 1000 > source priority 500
        assert_eq!(editor.value, "nvim");
    }

    // --- Conflict resolution: files with different content ---

    #[test]
    fn higher_priority_source_wins_file_content() {
        let local = ResolvedProfile {
            layers: vec![ProfileLayer {
                source: "local".into(),
                profile_name: "default".into(),
                priority: 1000,
                policy: LayerPolicy::Local,
                spec: ProfileSpec::default(),
            }],
            merged: MergedProfile::default(),
        };
        let low = CompositionInput {
            source_name: "low-src".into(),
            priority: 200,
            policy: ConfigSourcePolicy {
                recommended: PolicyItems {
                    files: vec![ManagedFileSpec {
                        source: "low/gitconfig".into(),
                        target: "~/.gitconfig".into(),
                        strategy: None,
                        private: false,
                        origin: None,
                        encryption: None,
                        permissions: None,
                    }],
                    ..Default::default()
                },
                ..Default::default()
            },
            constraints: Default::default(),
            layers: vec![],
            subscription: SubscriptionConfig {
                accept_recommended: true,
                ..Default::default()
            },
        };
        let high = CompositionInput {
            source_name: "high-src".into(),
            priority: 800,
            policy: ConfigSourcePolicy {
                recommended: PolicyItems {
                    files: vec![ManagedFileSpec {
                        source: "high/gitconfig".into(),
                        target: "~/.gitconfig".into(),
                        strategy: None,
                        private: false,
                        origin: None,
                        encryption: None,
                        permissions: None,
                    }],
                    ..Default::default()
                },
                ..Default::default()
            },
            constraints: Default::default(),
            layers: vec![],
            subscription: SubscriptionConfig {
                accept_recommended: true,
                ..Default::default()
            },
        };

        let result = compose(&local, &[low, high]).unwrap();
        let file = result
            .resolved
            .merged
            .files
            .managed
            .iter()
            .find(|f| f.target.to_string_lossy().contains("gitconfig"))
            .expect("gitconfig file must exist in merged output");
        // Higher priority source's file content wins
        assert_eq!(file.source, "high/gitconfig");
    }

    // --- Merging with an empty source ---

    #[test]
    fn merging_with_empty_source_does_not_affect_result() {
        let local = make_local_profile();
        let result_no_sources = compose(&local, &[]).unwrap();

        let empty_source = CompositionInput {
            source_name: "empty".into(),
            priority: 500,
            policy: ConfigSourcePolicy::default(),
            constraints: Default::default(),
            layers: vec![],
            subscription: SubscriptionConfig::default(),
        };
        let result_with_empty = compose(&local, &[empty_source]).unwrap();

        let yaml_without = serde_yaml::to_string(&result_no_sources.resolved.merged).unwrap();
        let yaml_with = serde_yaml::to_string(&result_with_empty.resolved.merged).unwrap();
        assert_eq!(
            yaml_without, yaml_with,
            "Empty source must not change the merged output"
        );
    }

    // --- Edge case: single source ---

    #[test]
    fn single_source_merges_correctly() {
        let local = ResolvedProfile {
            layers: vec![ProfileLayer {
                source: "local".into(),
                profile_name: "default".into(),
                priority: 1000,
                policy: LayerPolicy::Local,
                spec: ProfileSpec {
                    packages: Some(PackagesSpec {
                        brew: Some(BrewSpec {
                            formulae: vec!["git".into()],
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            }],
            merged: MergedProfile {
                packages: PackagesSpec {
                    brew: Some(BrewSpec {
                        formulae: vec!["git".into()],
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                ..Default::default()
            },
        };
        let source = CompositionInput {
            source_name: "tools".into(),
            priority: 500,
            policy: ConfigSourcePolicy {
                recommended: PolicyItems {
                    packages: Some(PackagesSpec {
                        brew: Some(BrewSpec {
                            formulae: vec!["fzf".into()],
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    env: vec![EnvVar {
                        name: "FZF_DEFAULT_OPTS".into(),
                        value: "--height 40%".into(),
                    }],
                    ..Default::default()
                },
                ..Default::default()
            },
            constraints: Default::default(),
            layers: vec![],
            subscription: SubscriptionConfig {
                accept_recommended: true,
                ..Default::default()
            },
        };

        let result = compose(&local, &[source]).unwrap();
        let brew = result.resolved.merged.packages.brew.as_ref().unwrap();
        assert!(brew.formulae.contains(&"git".to_string()));
        assert!(brew.formulae.contains(&"fzf".to_string()));
        assert!(
            result
                .resolved
                .merged
                .env
                .iter()
                .any(|e| e.name == "FZF_DEFAULT_OPTS")
        );
    }

    // --- Edge case: overlapping packages across sources ---

    #[test]
    fn overlapping_packages_are_unioned_not_duplicated() {
        let local = ResolvedProfile {
            layers: vec![ProfileLayer {
                source: "local".into(),
                profile_name: "default".into(),
                priority: 1000,
                policy: LayerPolicy::Local,
                spec: ProfileSpec {
                    packages: Some(PackagesSpec {
                        brew: Some(BrewSpec {
                            formulae: vec!["git".into(), "curl".into()],
                            ..Default::default()
                        }),
                        pipx: vec!["black".into()],
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            }],
            merged: MergedProfile {
                packages: PackagesSpec {
                    brew: Some(BrewSpec {
                        formulae: vec!["git".into(), "curl".into()],
                        ..Default::default()
                    }),
                    pipx: vec!["black".into()],
                    ..Default::default()
                },
                ..Default::default()
            },
        };
        let source_a = CompositionInput {
            source_name: "alpha".into(),
            priority: 300,
            policy: ConfigSourcePolicy {
                recommended: PolicyItems {
                    packages: Some(PackagesSpec {
                        brew: Some(BrewSpec {
                            formulae: vec!["git".into(), "ripgrep".into()],
                            ..Default::default()
                        }),
                        pipx: vec!["ruff".into(), "black".into()],
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                ..Default::default()
            },
            constraints: Default::default(),
            layers: vec![],
            subscription: SubscriptionConfig {
                accept_recommended: true,
                ..Default::default()
            },
        };
        let source_b = CompositionInput {
            source_name: "beta".into(),
            priority: 700,
            policy: ConfigSourcePolicy {
                recommended: PolicyItems {
                    packages: Some(PackagesSpec {
                        brew: Some(BrewSpec {
                            formulae: vec!["ripgrep".into(), "jq".into()],
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                ..Default::default()
            },
            constraints: Default::default(),
            layers: vec![],
            subscription: SubscriptionConfig {
                accept_recommended: true,
                ..Default::default()
            },
        };

        let result = compose(&local, &[source_a, source_b]).unwrap();
        let brew = result.resolved.merged.packages.brew.as_ref().unwrap();

        // All unique formulae present
        assert!(brew.formulae.contains(&"git".to_string()));
        assert!(brew.formulae.contains(&"curl".to_string()));
        assert!(brew.formulae.contains(&"ripgrep".to_string()));
        assert!(brew.formulae.contains(&"jq".to_string()));

        // No duplicates: count occurrences of "git" and "ripgrep"
        assert_eq!(
            brew.formulae.iter().filter(|f| *f == "git").count(),
            1,
            "git must appear exactly once"
        );
        assert_eq!(
            brew.formulae.iter().filter(|f| *f == "ripgrep").count(),
            1,
            "ripgrep must appear exactly once"
        );

        // pipx: black should not be duplicated
        let pipx = &result.resolved.merged.packages.pipx;
        assert!(pipx.contains(&"black".to_string()));
        assert!(pipx.contains(&"ruff".to_string()));
        assert_eq!(
            pipx.iter().filter(|p| *p == "black").count(),
            1,
            "black must appear exactly once"
        );
    }

    // --- Edge case: source_env populated correctly ---

    #[test]
    fn source_env_tracks_per_source_env_vars() {
        let local = make_local_profile();
        let source = CompositionInput {
            source_name: "corp".into(),
            priority: 500,
            policy: ConfigSourcePolicy {
                recommended: PolicyItems {
                    env: vec![EnvVar {
                        name: "CORP_VAR".into(),
                        value: "corp-value".into(),
                    }],
                    ..Default::default()
                },
                ..Default::default()
            },
            constraints: Default::default(),
            layers: vec![ProfileLayer {
                source: "corp".into(),
                profile_name: "corp/base".into(),
                priority: 500,
                policy: LayerPolicy::Recommended,
                spec: ProfileSpec {
                    env: vec![EnvVar {
                        name: "LAYER_VAR".into(),
                        value: "from-layer".into(),
                    }],
                    ..Default::default()
                },
            }],
            subscription: SubscriptionConfig {
                accept_recommended: true,
                ..Default::default()
            },
        };

        let result = compose(&local, &[source]).unwrap();
        let corp_env = result.source_env.get("corp").expect("corp env must exist");
        // source_env is built from input.layers (not policy), so it should contain LAYER_VAR
        assert!(corp_env.iter().any(|e| e.name == "LAYER_VAR"));
    }

    // --- Edge case: compose with empty local profile ---

    #[test]
    fn compose_with_empty_local_and_no_sources() {
        let local = ResolvedProfile {
            layers: vec![],
            merged: MergedProfile::default(),
        };
        let result = compose(&local, &[]).unwrap();
        assert!(result.resolved.merged.env.is_empty());
        assert!(result.resolved.merged.modules.is_empty());
        assert!(result.resolved.merged.files.managed.is_empty());
        assert!(result.conflicts.is_empty());
    }

    // --- Edge case: aliases with same name across priorities ---

    #[test]
    fn higher_priority_source_wins_alias() {
        let local = ResolvedProfile {
            layers: vec![ProfileLayer {
                source: "local".into(),
                profile_name: "default".into(),
                priority: 1000,
                policy: LayerPolicy::Local,
                spec: ProfileSpec {
                    aliases: vec![ShellAlias {
                        name: "ll".into(),
                        command: "ls -la".into(),
                    }],
                    ..Default::default()
                },
            }],
            merged: MergedProfile {
                aliases: vec![ShellAlias {
                    name: "ll".into(),
                    command: "ls -la".into(),
                }],
                ..Default::default()
            },
        };
        let source = CompositionInput {
            source_name: "corp".into(),
            priority: 500,
            policy: ConfigSourcePolicy {
                recommended: PolicyItems {
                    aliases: vec![ShellAlias {
                        name: "ll".into(),
                        command: "exa -la".into(),
                    }],
                    ..Default::default()
                },
                ..Default::default()
            },
            constraints: Default::default(),
            layers: vec![],
            subscription: SubscriptionConfig {
                accept_recommended: true,
                ..Default::default()
            },
        };

        let result = compose(&local, &[source]).unwrap();
        let ll = result
            .resolved
            .merged
            .aliases
            .iter()
            .find(|a| a.name == "ll")
            .expect("ll alias must exist");
        // Local (priority 1000) processed after source (500), so local wins
        assert_eq!(ll.command, "ls -la");
    }

    #[test]
    fn encryption_module_file_matching_required_target_without_encryption_is_error() {
        // Module files come through as ProfileSpec files just like profile files;
        // validate_constraints sees them the same way.
        let constraints = make_encryption_constraint(&["~/.config/secrets/*"], None);
        let spec = ProfileSpec {
            files: Some(FilesSpec {
                managed: vec![
                    // First file does NOT match the pattern — OK
                    ManagedFileSpec {
                        source: "files/.zshrc".into(),
                        target: "~/.zshrc".into(),
                        strategy: None,
                        private: false,
                        origin: None,
                        encryption: None,
                        permissions: None,
                    },
                    // Second file MATCHES the pattern and has no encryption — error
                    ManagedFileSpec {
                        source: "files/api-key".into(),
                        target: "~/.config/secrets/api-key".into(),
                        strategy: None,
                        private: false,
                        origin: None,
                        encryption: None,
                        permissions: None,
                    },
                ],
                ..Default::default()
            }),
            ..Default::default()
        };
        let result = validate_constraints("corp", &constraints, &spec);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("~/.config/secrets/api-key"), "msg: {msg}");
    }
}
