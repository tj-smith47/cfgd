use serde::{Deserialize, Serialize};

use super::sync_secrets::{NotifyConfig, SyncConfig};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub reconcile: Option<ReconcileConfig>,
    #[serde(default)]
    pub sync: Option<SyncConfig>,
    #[serde(default)]
    pub notify: Option<NotifyConfig>,
    /// Mirror daemon log output into the Windows Event Log under the `cfgd`
    /// source, in addition to the default file appender at
    /// `%LOCALAPPDATA%\cfgd\daemon.log`. No effect on Unix. Read by
    /// `cfgd daemon install` to bake `--enable-event-log` into the service
    /// binPath; changes require reinstalling the service to take effect.
    #[serde(default)]
    pub windows_event_log: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconcileConfig {
    #[serde(default = "default_reconcile_interval")]
    pub interval: String,
    #[serde(default)]
    pub on_change: bool,
    #[serde(default)]
    pub auto_apply: bool,
    #[serde(default)]
    pub policy: Option<AutoApplyPolicyConfig>,
    /// Policy for daemon auto-reconciliation of detected drift.
    /// `Auto` = silently apply (must opt-in), `NotifyOnly` = notify but don't
    /// apply (safe default), `Prompt` = future interactive approval.
    #[serde(default)]
    pub drift_policy: DriftPolicy,
    /// Per-module or per-profile reconcile overrides (kustomize-style patches).
    /// Each patch targets a specific Module or Profile by name and overrides
    /// individual reconcile fields. Precedence: Module patch > Profile patch > global.
    #[serde(default)]
    pub patches: Vec<ReconcilePatch>,
}

/// A kustomize-style reconcile patch targeting a specific module or profile.
/// When `name` is omitted, the patch applies to all entities of the given kind.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconcilePatch {
    pub kind: ReconcilePatchKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_apply: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drift_policy: Option<DriftPolicy>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReconcilePatchKind {
    Module,
    Profile,
}

/// Daemon drift reconciliation policy. PascalCase values match K8s enum conventions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DriftPolicy {
    /// Apply drift corrections automatically (current behavior, now opt-in).
    Auto,
    /// Notify and record drift, but do not apply. User must run `cfgd apply`.
    #[default]
    NotifyOnly,
    /// Future: notify with actionable prompt.
    Prompt,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoApplyPolicyConfig {
    #[serde(default = "default_policy_notify")]
    pub new_recommended: PolicyAction,
    #[serde(default = "default_policy_ignore")]
    pub new_optional: PolicyAction,
    #[serde(default = "default_policy_notify")]
    pub locked_conflict: PolicyAction,
}

impl Default for AutoApplyPolicyConfig {
    fn default() -> Self {
        Self {
            new_recommended: PolicyAction::Notify,
            new_optional: PolicyAction::Ignore,
            locked_conflict: PolicyAction::Notify,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolicyAction {
    Notify,
    Accept,
    Reject,
    Ignore,
}

fn default_policy_notify() -> PolicyAction {
    PolicyAction::Notify
}

fn default_policy_ignore() -> PolicyAction {
    PolicyAction::Ignore
}

fn default_reconcile_interval() -> String {
    "5m".to_string()
}
