// Composition — multi-source merge engine with policy enforcement
// Dependency rules: depends only on config/, errors/. Must NOT import
// files/, packages/, secrets/, reconciler/, daemon/, providers/.

use std::collections::HashMap;

use crate::config::{
    ConfigSourcePolicy, EnvVar, ProfileLayer, ResolvedProfile, SourceConstraints, SourceSpec,
};

mod constraints;
mod engine;
mod layers;
mod merge;
mod packages;
mod permissions;
mod policy;
mod record;

#[cfg(test)]
mod tests;

pub use constraints::{check_locked_violations, validate_constraints};
pub use engine::compose;
pub use packages::merge_packages;
pub use permissions::{PermissionChange, detect_permission_changes};

// Re-export sibling submodule items at the parent level so the externalized
// tests submodule can reach them via `super::*`. The `#[cfg(test)]` guard
// keeps these at module-private scope and only compiles them when tests run.
#[cfg(test)]
use {constraints::*, layers::*, packages::*, permissions::*, policy::*, record::*};

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
