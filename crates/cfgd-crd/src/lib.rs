//! cfgd Custom Resource Definition spec types.
//!
//! This crate hosts the `cfgd.io/v1alpha1` CRD spec types (`MachineConfig`,
//! `ConfigPolicy`, `ClusterConfigPolicy`, `DriftAlert`, `Module`), their
//! `schemars`-derived JSON schemas, and the cross-field `validate()` impls used
//! by both the admission webhook and the CLI. It sits at the bottom of the
//! workspace dependency graph (depended on by `cfgd-core`), so it carries no
//! Kubernetes client/runtime, no HTTP server, and no telemetry — only the
//! schema-bearing types.

use std::collections::BTreeMap;

use kube::CustomResource;
use schemars::JsonSchema;
use semver::VersionReq;
use serde::{Deserialize, Serialize};

/// Ceiling on the `nonCompliantMachines` list a policy status enumerates.
///
/// A status object every operator replica watches has to stay well inside
/// etcd's ~1.5 MiB limit, and a policy violated fleet-wide would otherwise
/// carry one entry per machine. At the RFC 1123 worst case (a 253-character
/// namespace and name) 500 entries is ~250 KiB, which leaves the conditions
/// list and the rest of the object ample room.
///
/// The cap is on the ENUMERATION only: `nonCompliantCount` remains exact, so no
/// number a user reads is ever wrong. The truncation is applied to a sorted
/// list, so which machines fall outside is deterministic rather than
/// per-reconcile arbitrary — a machine beyond the cap is absent from the
/// transition memory and therefore re-fires its `PolicyViolation` event on each
/// evaluation, which is the documented degradation at that scale.
pub const MAX_NON_COMPLIANT_MACHINES: usize = 500;

// ---------------------------------------------------------------------------
// MachineConfig
// ---------------------------------------------------------------------------

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(
    group = "cfgd.io",
    version = "v1alpha1",
    kind = "MachineConfig",
    namespaced,
    status = "MachineConfigStatus",
    shortname = "mc",
    category = "cfgd",
    printcolumn = r#"{"name": "Hostname", "type": "string", "jsonPath": ".spec.hostname"}"#,
    printcolumn = r#"{"name": "Profile", "type": "string", "jsonPath": ".spec.profile"}"#,
    printcolumn = r#"{"name": "Reconciled", "type": "string", "jsonPath": ".status.conditions[?(@.type==\"Reconciled\")].status"}"#,
    printcolumn = r#"{"name": "Drift", "type": "string", "jsonPath": ".status.conditions[?(@.type==\"DriftDetected\")].status"}"#,
    printcolumn = r#"{"name": "Age", "type": "date", "jsonPath": ".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct MachineConfigSpec {
    /// Hostname of the machine this document describes. The agent reconciles
    /// only the MachineConfig whose hostname matches its own.
    pub hostname: String,
    /// Name of the profile the machine reconciles against. Resolved on the
    /// machine, from its own config directory or from a subscribed source.
    pub profile: String,
    /// Modules to install on the machine, each naming a cluster-scoped
    /// `Module` resource.
    #[serde(default)]
    pub module_refs: Vec<ModuleRef>,
    /// Packages to install on top of whatever the profile declares, each
    /// optionally version-pinned.
    #[serde(default)]
    pub packages: Vec<PackageRef>,
    /// Files to write on the machine, either inline or fetched from a source
    /// path.
    #[serde(default)]
    pub files: Vec<FileSpec>,
    /// System configurator settings to apply, keyed by
    /// `<configurator>.<setting>` (e.g. `sysctl.net.ipv4.ip_forward`). Empty,
    /// no system settings are reconciled.
    #[serde(default)]
    pub system_settings: BTreeMap<String, serde_json::Value>,
}

/// Reference to a package with optional version pin.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PackageRef {
    /// Package name as the machine's own package manager knows it.
    pub name: String,
    /// Exact version to pin to. Omitted, whatever the manager currently offers
    /// is installed and left alone once present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Reference to a module that should be installed on the machine.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModuleRef {
    /// Name of the cluster-scoped `Module` resource to install.
    pub name: String,
    /// Whether a failure to resolve or install this module fails the whole
    /// reconcile. Default: `false`, so a missing module is reported and the
    /// rest of the machine still converges.
    #[serde(default)]
    pub required: bool,
}

/// A file the operator writes on a managed machine.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FileSpec {
    /// Destination path on the machine. A leading `~` expands to the home
    /// directory of the user the agent runs as.
    pub path: String,
    /// Literal file body, written as-is. Mutually exclusive with `source`.
    pub content: Option<String>,
    /// Path the body is read from instead of `content`, resolved against the
    /// machine's config directory.
    pub source: Option<String>,
    /// Octal permission bits applied after the write. Default: `0644`.
    #[serde(default = "default_mode")]
    pub mode: String,
}

fn default_mode() -> String {
    "0644".to_string()
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MachineConfigStatus {
    pub last_reconciled: Option<String>,
    #[serde(default)]
    pub observed_generation: Option<i64>,
    #[serde(default)]
    pub conditions: Vec<Condition>,
    /// Reported installed versions keyed by package name (e.g. {"kubectl": "1.28.3"}).
    #[serde(default)]
    pub package_versions: BTreeMap<String, String>,
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Condition {
    #[serde(rename = "type")]
    pub condition_type: String,
    pub status: String,
    pub reason: String,
    pub message: String,
    pub last_transition_time: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
}

// ---------------------------------------------------------------------------
// ConfigPolicy
// ---------------------------------------------------------------------------

/// Kubernetes-style label selector with match_labels and match_expressions.
#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LabelSelector {
    /// Labels a resource must carry verbatim to match. Every entry must match;
    /// an empty map matches everything.
    #[serde(default)]
    pub match_labels: BTreeMap<String, String>,
    /// Set-based requirements a resource must satisfy, evaluated alongside
    /// `matchLabels`. Every requirement must hold.
    #[serde(default)]
    pub match_expressions: Vec<LabelSelectorRequirement>,
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub enum SelectorOperator {
    In,
    NotIn,
    Exists,
    DoesNotExist,
}

/// A single requirement for label selector expressions.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LabelSelectorRequirement {
    /// Label key this requirement tests.
    pub key: String,
    /// How `values` is compared against the key: `In`, `NotIn`, `Exists`, or
    /// `DoesNotExist`.
    pub operator: SelectorOperator,
    /// Values the key is tested against. Required for `In` and `NotIn`, and
    /// must be empty for `Exists` and `DoesNotExist`.
    #[serde(default)]
    pub values: Vec<String>,
}

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[kube(
    group = "cfgd.io",
    version = "v1alpha1",
    kind = "ConfigPolicy",
    namespaced,
    status = "ConfigPolicyStatus",
    shortname = "cpol",
    category = "cfgd",
    printcolumn = r#"{"name": "Compliant", "type": "integer", "jsonPath": ".status.compliantCount"}"#,
    printcolumn = r#"{"name": "NonCompliant", "type": "integer", "jsonPath": ".status.nonCompliantCount"}"#,
    printcolumn = r#"{"name": "Enforced", "type": "string", "jsonPath": ".status.conditions[?(@.type==\"Enforced\")].status"}"#,
    printcolumn = r#"{"name": "Age", "type": "date", "jsonPath": ".metadata.creationTimestamp"}"#
)]
// `deny_unknown_fields` is intentionally OFF: schemars 0.8 maps it to
// `additionalProperties: false` in the generated JSON schema, which the k8s
// CRD structural-schema validator rejects when set alongside `properties:`
// (mutually exclusive). K8s already rejects unknown fields via the `properties`
// allowlist at admission time, so serde-side strictness is redundant here.
#[serde(rename_all = "camelCase")]
pub struct ConfigPolicySpec {
    /// Modules every selected MachineConfig must carry. A machine missing one
    /// is counted non-compliant.
    #[serde(default)]
    pub required_modules: Vec<ModuleRef>,
    /// Modules staged as debug-only (CSI volume without volumeMount on declared containers).
    #[serde(default)]
    pub debug_modules: Vec<ModuleRef>,
    /// Packages every selected MachineConfig must declare, each optionally
    /// version-pinned.
    #[serde(default)]
    pub packages: Vec<PackageRef>,
    /// System settings every selected MachineConfig must declare, keyed the
    /// same way as `MachineConfig.spec.systemSettings`.
    #[serde(default)]
    pub settings: BTreeMap<String, serde_json::Value>,
    /// Which MachineConfigs in this namespace the policy applies to. Empty,
    /// it applies to all of them.
    #[serde(default)]
    pub target_selector: LabelSelector,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConfigPolicyStatus {
    pub compliant_count: u32,
    pub non_compliant_count: u32,
    /// `namespace/name` of the MachineConfigs currently violating this policy,
    /// sorted and capped at [`MAX_NON_COMPLIANT_MACHINES`]. Persisted so a
    /// `PolicyViolation` event fires on the transition into violation rather
    /// than once per observation — an in-process memory would re-announce every
    /// machine after an operator restart. `nonCompliantCount` is the exact
    /// total and is never capped.
    #[serde(default)]
    #[schemars(length(max = MAX_NON_COMPLIANT_MACHINES))]
    pub non_compliant_machines: Vec<String>,
    #[serde(default)]
    pub conditions: Vec<Condition>,
}

// ---------------------------------------------------------------------------
// DriftAlert
// ---------------------------------------------------------------------------

/// Typed reference to a MachineConfig resource.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MachineConfigReference {
    /// Name of the referenced MachineConfig.
    pub name: String,
    /// Namespace the MachineConfig lives in. Omitted, the referring
    /// resource's own namespace is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
}

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(
    group = "cfgd.io",
    version = "v1alpha1",
    kind = "DriftAlert",
    namespaced,
    status = "DriftAlertStatus",
    shortname = "da",
    category = "cfgd",
    printcolumn = r#"{"name": "Device", "type": "string", "jsonPath": ".spec.deviceId"}"#,
    printcolumn = r#"{"name": "Severity", "type": "string", "jsonPath": ".spec.severity"}"#,
    printcolumn = r#"{"name": "Resolved", "type": "string", "jsonPath": ".status.conditions[?(@.type==\"Resolved\")].status"}"#,
    printcolumn = r#"{"name": "Age", "type": "date", "jsonPath": ".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct DriftAlertSpec {
    /// Identifier of the device that reported the drift, as it enrolled.
    pub device_id: String,
    /// The MachineConfig whose desired state the device diverged from.
    pub machine_config_ref: MachineConfigReference,
    /// One entry per diverging field, each carrying what was expected and what
    /// was found.
    #[serde(default)]
    pub drift_details: Vec<DriftDetail>,
    /// How serious the divergence is: `Low`, `Medium`, `High`, or `Critical`.
    /// Drives alert routing rather than any automatic remediation.
    pub severity: DriftSeverity,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DriftAlertStatus {
    pub detected_at: Option<String>,
    pub resolved_at: Option<String>,
    #[serde(default)]
    pub conditions: Vec<Condition>,
}

/// One diverging field in a drift report.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DriftDetail {
    /// Resource id of the field that diverged (e.g.
    /// `system:sysctl.net.ipv4.ip_forward`).
    pub field: String,
    /// The value the MachineConfig declares.
    pub expected: String,
    /// The value the device actually reported.
    pub actual: String,
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub enum DriftSeverity {
    Low,
    Medium,
    High,
    Critical,
}

// ---------------------------------------------------------------------------
// ClusterConfigPolicy
// ---------------------------------------------------------------------------

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[kube(
    group = "cfgd.io",
    version = "v1alpha1",
    kind = "ClusterConfigPolicy",
    status = "ClusterConfigPolicyStatus",
    shortname = "ccpol",
    category = "cfgd",
    printcolumn = r#"{"name": "Compliant", "type": "integer", "jsonPath": ".status.compliantCount"}"#,
    printcolumn = r#"{"name": "NonCompliant", "type": "integer", "jsonPath": ".status.nonCompliantCount"}"#,
    printcolumn = r#"{"name": "Enforced", "type": "string", "jsonPath": ".status.conditions[?(@.type==\"Enforced\")].status"}"#,
    printcolumn = r#"{"name": "Age", "type": "date", "jsonPath": ".metadata.creationTimestamp"}"#
)]
// See note on ConfigPolicySpec: omitting `deny_unknown_fields` so schemars
// doesn't emit `additionalProperties: false` (k8s rejects it alongside
// `properties:`). K8s admission already gatekeeps unknown fields.
#[serde(rename_all = "camelCase")]
pub struct ClusterConfigPolicySpec {
    /// Select which namespaces this cluster policy applies to. Empty, it
    /// applies to every namespace in the cluster.
    #[serde(default)]
    pub namespace_selector: LabelSelector,
    /// Modules every MachineConfig in a selected namespace must carry.
    #[serde(default)]
    pub required_modules: Vec<ModuleRef>,
    /// Modules staged as debug-only across matching namespaces.
    #[serde(default)]
    pub debug_modules: Vec<ModuleRef>,
    /// Packages every MachineConfig in a selected namespace must declare.
    #[serde(default)]
    pub packages: Vec<PackageRef>,
    /// System settings every MachineConfig in a selected namespace must
    /// declare, keyed the same way as `MachineConfig.spec.systemSettings`.
    #[serde(default)]
    pub settings: BTreeMap<String, serde_json::Value>,
    /// Cluster-wide rules governing which modules may be admitted at all.
    #[serde(default)]
    pub security: SecurityPolicy,
}

/// Provenance rules a `Module` must satisfy to be admitted.
#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SecurityPolicy {
    /// Registry prefixes a module artifact may be pulled from; a trailing `*`
    /// or `/` widens the match. Empty, any registry is accepted.
    #[serde(default)]
    pub trusted_registries: Vec<String>,
    /// Whether a module carrying an unsigned OCI artifact may be admitted.
    /// Default: `false` — creating any ClusterConfigPolicy at all starts
    /// rejecting unsigned modules.
    #[serde(default)]
    pub allow_unsigned: bool,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClusterConfigPolicyStatus {
    pub compliant_count: u32,
    pub non_compliant_count: u32,
    /// `namespace/name` of the MachineConfigs currently violating this policy,
    /// sorted and capped at [`MAX_NON_COMPLIANT_MACHINES`]. Persisted so a
    /// `PolicyViolation` event fires on the transition into violation rather
    /// than once per observation — an in-process memory would re-announce every
    /// machine after an operator restart. `nonCompliantCount` is the exact
    /// total and is never capped.
    #[serde(default)]
    #[schemars(length(max = MAX_NON_COMPLIANT_MACHINES))]
    pub non_compliant_machines: Vec<String>,
    #[serde(default)]
    pub conditions: Vec<Condition>,
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

/// An entry in a Module's package list with optional per-platform overrides.
#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PackageEntry {
    /// Default package name, used on every platform with no override below.
    pub name: String,
    /// Per-platform package name overrides (e.g. {"brew": "gnu-sed", "apt": "sed"}).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub platforms: BTreeMap<String, String>,
}

/// A file managed by a Module.
#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModuleFileSpec {
    /// Path to the file inside the module artifact.
    pub source: String,
    /// Destination path the file is deployed to inside the pod or on the
    /// machine.
    pub target: String,
}

/// Scripts that run during module lifecycle.
#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModuleScripts {
    /// Command run once after the module's files and packages are in place.
    /// Omitted, nothing runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_apply: Option<String>,
}

/// An environment variable set by a Module.
#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModuleEnvVar {
    /// Variable name, as exported into the container or shell environment.
    pub name: String,
    /// Value assigned to the variable.
    pub value: String,
    /// Append to the variable's existing value with the platform's path
    /// separator instead of replacing it. Default: `false`.
    #[serde(default)]
    pub append: bool,
}

/// Cosign configuration for OCI artifact signature verification.
#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CosignSignature {
    /// PEM-encoded public key for static key verification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_key: Option<String>,
    /// Enable keyless verification via Fulcio/Rekor (OIDC identity-based).
    #[serde(default)]
    pub keyless: bool,
    /// Certificate identity pattern for keyless verification (regex).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate_identity: Option<String>,
    /// Certificate OIDC issuer pattern for keyless verification (regex).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate_oidc_issuer: Option<String>,
}

/// Signature configuration for a Module's OCI artifact.
#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModuleSignature {
    /// How the artifact's cosign signature is verified. Omitted, no signature
    /// is required of this module.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cosign: Option<CosignSignature>,
}

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[kube(
    group = "cfgd.io",
    version = "v1alpha1",
    kind = "Module",
    status = "ModuleStatus",
    shortname = "mod",
    category = "cfgd",
    printcolumn = r#"{"name": "Artifact", "type": "string", "jsonPath": ".spec.ociArtifact"}"#,
    printcolumn = r#"{"name": "Signature", "type": "string", "jsonPath": ".status.signature"}"#,
    printcolumn = r#"{"name": "Platforms", "type": "string", "jsonPath": ".status.platformsSummary"}"#,
    printcolumn = r#"{"name": "Age", "type": "date", "jsonPath": ".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct ModuleSpec {
    /// Packages this module installs, each with optional per-platform name
    /// overrides.
    #[serde(default)]
    pub packages: Vec<PackageEntry>,
    /// Files this module deploys out of its artifact.
    #[serde(default)]
    pub files: Vec<ModuleFileSpec>,
    /// Lifecycle commands run around the module's deployment.
    #[serde(default)]
    pub scripts: ModuleScripts,
    /// Environment variables this module contributes to containers that mount
    /// it.
    #[serde(default)]
    pub env: Vec<ModuleEnvVar>,
    /// Names of other `Module` resources that must be applied first.
    #[serde(default)]
    pub depends: Vec<String>,
    /// OCI reference the module's content is pulled from
    /// (`ghcr.io/acme/nvim:1.2.0`). Omitted, the module carries its content
    /// inline and nothing is fetched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oci_artifact: Option<String>,
    /// How the artifact's provenance is verified before it is admitted.
    /// Omitted, verification is governed by the cluster policy alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<ModuleSignature>,
    /// Controls how the module is mounted in pods.
    /// `Always` (default): CSI volume + volumeMount on all declared containers + env vars.
    /// `Debug`: CSI volume only — no volumeMount or env on declared containers.
    ///   Only accessible via ephemeral debug containers (`kubectl cfgd debug`).
    #[serde(default)]
    pub mount_policy: MountPolicy,
}

/// Controls how a module is exposed to pod containers.
#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq, Eq, JsonSchema)]
pub enum MountPolicy {
    /// Mount into all declared containers with volumeMount and env vars.
    #[default]
    Always,
    /// Stage the CSI volume on the pod but do not mount into declared containers.
    /// Only accessible via ephemeral debug containers.
    Debug,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModuleStatus {
    /// The `metadata.generation` every other field here was computed from. A
    /// status whose `observedGeneration` is behind `metadata.generation`
    /// describes the PREVIOUS spec, which is what lets a caller wait for a
    /// verdict about the spec it just applied instead of reading the one it
    /// replaced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_artifact: Option<String>,
    #[serde(default)]
    pub available_platforms: Vec<String>,
    /// `availablePlatforms` rendered as one comma-joined string for the
    /// `Platforms` printer column, and the only field that column may be bound
    /// to: a column resolving to an array prints the Go rendering of the
    /// slice, so an empty one reads as the literal `[]` where an absent value
    /// leaves the cell empty. Absent when no platform is known, so the
    /// JSONPath resolves to nothing and the cell stays blank.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platforms_summary: Option<String>,
    #[serde(default)]
    pub verified: bool,
    /// The signature verdict as ONE word (`verified` / `unverified` /
    /// `unsigned` / `unknown`), and the only field the `Signature` printer
    /// column may be bound to. `verified` is the same verdict as a raw bool,
    /// which reads as
    /// `true` in a column beside a `kubectl cfgd status` row saying
    /// `(verified)` about the same module — one fact, two vocabularies. Absent
    /// when no reconcile has written it, so the JSONPath resolves to nothing
    /// and the cell stays blank.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// Digest of the cosign signature (if verified).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature_digest: Option<String>,
    /// Attestation types found on the artifact (e.g. "slsaprovenance1").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attestations: Vec<String>,
    #[serde(default)]
    pub conditions: Vec<Condition>,
}

/// A module whose declared signature was checked against its key and held.
pub const SIGNATURE_VERIFIED: &str = "verified";
/// A module whose declared signature was checked and rejected — the artifact
/// carries no signature, or none the declared key accepts.
pub const SIGNATURE_UNVERIFIED: &str = "unverified";
/// A module that declares no signature at all — nothing to verify, which is a
/// different fact from a signature that failed.
pub const SIGNATURE_UNSIGNED: &str = "unsigned";
/// A module whose signature could not be checked at all: the verifier is
/// missing, or the artifact's registry could not be reached. A fact about the
/// CHECK, not about the signature, and the reason no surface may collapse it
/// into [`SIGNATURE_UNVERIFIED`].
pub const SIGNATURE_UNKNOWN: &str = "unknown";

impl ModuleStatus {
    /// The ONE derivation of [`ModuleStatus::platforms_summary`] from
    /// [`ModuleStatus::available_platforms`]. Every writer of the status goes
    /// through it, so the column and the list it summarizes cannot disagree.
    #[must_use]
    pub fn summarize_platforms(platforms: &[String]) -> Option<String> {
        (!platforms.is_empty()).then(|| platforms.join(", "))
    }

    /// The ONE derivation of [`ModuleStatus::signature`] — the word every
    /// surface naming a module's signature verdict prints, so the CRD column
    /// and `kubectl cfgd status` cannot spell one fact two ways.
    ///
    /// `declared` is whether the spec asks for a signature at all: a module
    /// that never claimed one is `unsigned`, not a module whose signature
    /// failed. Both collapse to `verified == false` on the wire, which is why
    /// the bool alone cannot answer this.
    ///
    /// The bool pair cannot express [`SIGNATURE_UNKNOWN`] — a check that never
    /// ran is neither of its two inputs — so a caller holding the outcome of a
    /// real check names that word directly and this door stays for the callers
    /// that hold only the two bools.
    #[must_use]
    pub fn signature_verdict(verified: bool, declared: bool) -> &'static str {
        match (verified, declared) {
            (true, _) => SIGNATURE_VERIFIED,
            (false, true) => SIGNATURE_UNVERIFIED,
            (false, false) => SIGNATURE_UNSIGNED,
        }
    }
}

// ---------------------------------------------------------------------------
// Shared validation
// ---------------------------------------------------------------------------

fn validate_policy_fields(
    packages: &[PackageRef],
    required_modules: &[ModuleRef],
    settings: &BTreeMap<String, serde_json::Value>,
) -> Vec<String> {
    let mut errors = Vec::new();
    for (i, pkg) in packages.iter().enumerate() {
        if pkg.name.is_empty() {
            errors.push(format!("spec.packages[{i}].name must not be empty"));
        }
        if let Some(ver) = &pkg.version
            && VersionReq::parse(ver).is_err()
        {
            errors.push(format!(
                "spec.packages[{i}].version '{ver}' is not a valid semver requirement"
            ));
        }
    }
    for (i, mr) in required_modules.iter().enumerate() {
        if mr.name.is_empty() {
            errors.push(format!("spec.requiredModules[{i}].name must not be empty"));
        }
    }
    for key in settings.keys() {
        if key.is_empty() {
            errors.push("spec.settings key must not be empty".to_string());
        }
    }
    errors
}

impl MachineConfigSpec {
    /// Validate the spec, returning all validation errors found.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();

        if self.hostname.trim().is_empty() {
            errors.push("spec.hostname must not be empty".to_string());
        }
        if self.profile.trim().is_empty() {
            errors.push("spec.profile must not be empty".to_string());
        }
        for (i, m) in self.module_refs.iter().enumerate() {
            if m.name.is_empty() {
                errors.push(format!("spec.moduleRefs[{i}].name must not be empty"));
            }
        }
        for (i, pkg) in self.packages.iter().enumerate() {
            if pkg.name.is_empty() {
                errors.push(format!("spec.packages[{i}].name must not be empty"));
            }
        }
        for (i, file) in self.files.iter().enumerate() {
            if file.path.is_empty() {
                errors.push(format!("spec.files[{i}].path must not be empty"));
            }
            if file.path.contains("..") {
                errors.push(format!(
                    "spec.files[{i}].path '{}' must not contain path traversal (..)",
                    file.path
                ));
            }
            if file.content.is_none() && file.source.is_none() {
                errors.push(format!(
                    "spec.files[{i}] ('{}') must have either content or source",
                    file.path
                ));
            }
            match u32::from_str_radix(&file.mode, 8) {
                Ok(mode) if mode > 0o7777 => {
                    errors.push(format!(
                        "spec.files[{i}].mode '{}' exceeds maximum 7777",
                        file.mode
                    ));
                }
                Err(_) => {
                    errors.push(format!(
                        "spec.files[{i}].mode '{}' is not valid octal",
                        file.mode
                    ));
                }
                _ => {}
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// A spec whose file path contains `..` traversal, for tests that exercise
    /// the cross-field validation rejection path.
    pub fn example_with_traversal_path() -> Self {
        MachineConfigSpec {
            hostname: "host1".to_string(),
            profile: "default".to_string(),
            module_refs: Vec::new(),
            packages: Vec::new(),
            files: vec![FileSpec {
                path: "/etc/../shadow".to_string(),
                content: Some("data".to_string()),
                source: None,
                mode: "0644".to_string(),
            }],
            system_settings: BTreeMap::new(),
        }
    }
}

impl ConfigPolicySpec {
    /// Validate the spec, returning all validation errors found.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let errors = validate_policy_fields(&self.packages, &self.required_modules, &self.settings);
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

impl ClusterConfigPolicySpec {
    /// Validate the spec, returning all validation errors found.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let errors = validate_policy_fields(&self.packages, &self.required_modules, &self.settings);
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

impl DriftAlertSpec {
    /// Validate the spec, returning all validation errors found.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();
        if self.device_id.is_empty() {
            errors.push("spec.deviceId must not be empty".to_string());
        }
        if self.machine_config_ref.name.is_empty() {
            errors.push("spec.machineConfigRef.name must not be empty".to_string());
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

impl ModuleSpec {
    /// Validate the spec, returning all validation errors found.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();
        for (i, pkg) in self.packages.iter().enumerate() {
            if pkg.name.is_empty() {
                errors.push(format!("spec.packages[{i}].name must not be empty"));
            }
        }
        for (i, dep) in self.depends.iter().enumerate() {
            if dep.is_empty() {
                errors.push(format!("spec.depends[{i}] must not be empty"));
            }
        }
        if let Some(ref oci) = self.oci_artifact
            && !is_valid_oci_reference(oci)
        {
            errors.push(format!(
                "spec.ociArtifact '{}' is not a valid OCI reference",
                oci
            ));
        }
        if let Some(ref sig) = self.signature
            && let Some(ref cosign) = sig.cosign
            && let Some(ref pk) = cosign.public_key
            && !is_valid_pem_public_key(pk)
        {
            errors.push(
                "spec.signature.cosign.publicKey is not a valid PEM-encoded public key".to_string(),
            );
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

/// Cross-field validation contract shared across CRD spec types.
///
/// Every spec type carries an inherent `validate()` that enforces its
/// cross-field invariants; this trait exposes that single implementation behind
/// one bound so generic dispatchers (the admission webhook and the unified
/// resource-kind registry) validate against the same logic and cannot diverge.
pub trait Validatable {
    /// Validate the spec, returning every cross-field error found.
    fn validate(&self) -> Result<(), Vec<String>>;
}

impl Validatable for MachineConfigSpec {
    fn validate(&self) -> Result<(), Vec<String>> {
        MachineConfigSpec::validate(self)
    }
}

impl Validatable for ConfigPolicySpec {
    fn validate(&self) -> Result<(), Vec<String>> {
        ConfigPolicySpec::validate(self)
    }
}

impl Validatable for ClusterConfigPolicySpec {
    fn validate(&self) -> Result<(), Vec<String>> {
        ClusterConfigPolicySpec::validate(self)
    }
}

impl Validatable for DriftAlertSpec {
    fn validate(&self) -> Result<(), Vec<String>> {
        DriftAlertSpec::validate(self)
    }
}

impl Validatable for ModuleSpec {
    fn validate(&self) -> Result<(), Vec<String>> {
        ModuleSpec::validate(self)
    }
}

/// Reject a Module whose `spec.oci_artifact` is set but carries no verifiable
/// signature, when `disallow_unsigned` is true.
///
/// This is the operator admission webhook's `disallowUnsigned` rule, which
/// calls straight into here, hoisted into this crate so any consumer of
/// `ModuleSpec` — the webhook, the CLI's `module push --apply` construction
/// tests — exercises the exact same predicate instead of an approximation, with
/// no dependency on the operator's server/gateway code.
pub fn check_unsigned_policy(spec: &ModuleSpec, disallow_unsigned: bool) -> Result<(), String> {
    if disallow_unsigned && spec.oci_artifact.is_some() {
        let has_signing = spec
            .signature
            .as_ref()
            .and_then(|s| s.cosign.as_ref())
            .map(|c| c.keyless || c.public_key.as_ref().is_some_and(|pk| !pk.is_empty()))
            .unwrap_or(false);
        if !has_signing {
            return Err(
                "unsigned modules are not allowed: configure spec.signature.cosign with publicKey or keyless"
                    .to_string(),
            );
        }
    }
    Ok(())
}

/// Validate the syntactic shape of an OCI artifact reference.
///
/// This is a self-contained validity predicate — the full parser that extracts
/// registry/repository/tag lives in `cfgd_core::oci::OciReference`, which
/// cannot be depended on here (this crate sits beneath `cfgd-core`). It mirrors
/// the parser's reject rules: empty input, embedded whitespace/control
/// characters, a bare `host:port` with no repository path, and a reference that
/// resolves to an empty repository.
pub fn is_valid_oci_reference(reference: &str) -> bool {
    if reference.is_empty()
        || reference
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
    {
        return false;
    }

    // Strip the tag/digest the same way the parser does, so a bare `host:port`
    // (numeric "tag" with no repository slash) is rejected.
    let name_part = if let Some((name, _digest)) = reference.split_once('@') {
        name
    } else if let Some((name, tag)) = reference.rsplit_once(':') {
        if tag.chars().all(|c| c.is_ascii_digit()) && !name.contains('/') {
            // host:port with no repository — invalid.
            return false;
        }
        if tag.contains('/') {
            // The colon was a port separator, not a tag separator.
            reference
        } else {
            name
        }
    } else {
        reference
    };

    // The repository component (everything after a registry host, if present)
    // must be non-empty. This mirrors the parser exactly: a single-segment name
    // (no '/') is always non-empty because the parser prefixes it with
    // `library/` (so even an empty name part, e.g. `@sha256:...`, resolves to a
    // valid `library/` repository); only the registry-host case (`host/rest`)
    // can yield an empty repository when `rest` is empty.
    let parts: Vec<&str> = name_part.splitn(2, '/').collect();
    if parts.len() == 1 {
        return true;
    }
    let first = parts[0];
    let repository = if first.contains('.') || first.contains(':') || first == "localhost" {
        parts[1]
    } else {
        name_part
    };

    !repository.is_empty()
}

/// Check if a string looks like a PEM-encoded public key.
pub fn is_valid_pem_public_key(key: &str) -> bool {
    let trimmed = key.trim();
    trimmed.starts_with("-----BEGIN PUBLIC KEY-----")
        && trimmed.ends_with("-----END PUBLIC KEY-----")
}

/// The `apiVersion` (`group/version`) shared by every cfgd CRD kind, read from
/// the kube-derived [`kube::Resource`] impl so it can never drift from the
/// `#[kube(group = …, version = …)]` attributes. `cfgd-core::API_VERSION` — the
/// string config parsing accepts — is guard-tested against this value, so a
/// `version = "v1beta1"` bump on the derives can't silently leave parsing
/// pinned to the old apiVersion.
#[must_use]
pub fn api_version() -> String {
    use kube::Resource;
    MachineConfig::api_version(&()).into_owned()
}

#[cfg(test)]
mod tests;
