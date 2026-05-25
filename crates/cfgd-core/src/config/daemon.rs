use serde::{Deserialize, Serialize};

use super::sync_secrets::{NotifyConfig, SyncConfig};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_apply_policy_default_values() {
        let policy = AutoApplyPolicyConfig::default();
        assert!(matches!(policy.new_recommended, PolicyAction::Notify));
        assert!(matches!(policy.new_optional, PolicyAction::Ignore));
        assert!(matches!(policy.locked_conflict, PolicyAction::Notify));
    }

    #[test]
    fn drift_policy_default_is_notify_only() {
        let dp = DriftPolicy::default();
        assert_eq!(dp, DriftPolicy::NotifyOnly);
    }

    #[test]
    fn reconcile_config_deserializes_with_defaults() {
        let yaml = "onChange: true";
        let config: ReconcileConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(config.on_change);
        assert!(!config.auto_apply);
        assert_eq!(config.interval, "5m");
        assert_eq!(config.drift_policy, DriftPolicy::NotifyOnly);
        assert!(config.patches.is_empty());
    }

    #[test]
    fn auto_apply_policy_deserializes_with_serde_defaults() {
        let yaml = "newRecommended: Accept";
        let config: AutoApplyPolicyConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(config.new_recommended, PolicyAction::Accept));
        assert!(matches!(config.new_optional, PolicyAction::Ignore));
        assert!(matches!(config.locked_conflict, PolicyAction::Notify));
    }

    #[test]
    fn daemon_config_deserializes_minimal() {
        let yaml = "enabled: true";
        let config: DaemonConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(config.enabled);
        assert!(config.reconcile.is_none());
        assert!(config.sync.is_none());
        assert!(config.notify.is_none());
        assert!(!config.windows_event_log);
    }

    #[test]
    fn reconcile_patch_deserializes() {
        let yaml = "kind: Module\nname: docker\ninterval: 10m\nautoApply: true\ndriftPolicy: Auto";
        let patch: ReconcilePatch = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(patch.kind, ReconcilePatchKind::Module);
        assert_eq!(patch.name.as_deref(), Some("docker"));
        assert_eq!(patch.interval.as_deref(), Some("10m"));
        assert_eq!(patch.auto_apply, Some(true));
        assert_eq!(patch.drift_policy, Some(DriftPolicy::Auto));
    }

    #[test]
    fn drift_policy_all_variants_deserialize() {
        let auto: DriftPolicy = serde_yaml::from_str("Auto").unwrap();
        let notify: DriftPolicy = serde_yaml::from_str("NotifyOnly").unwrap();
        let prompt: DriftPolicy = serde_yaml::from_str("Prompt").unwrap();
        assert_eq!(auto, DriftPolicy::Auto);
        assert_eq!(notify, DriftPolicy::NotifyOnly);
        assert_eq!(prompt, DriftPolicy::Prompt);
    }

    #[test]
    fn daemon_config_rejects_unknown_field() {
        let yaml = "enabled: true\nbogus: 1\n";
        let err = serde_yaml::from_str::<DaemonConfig>(yaml)
            .expect_err("expected deny_unknown_fields to reject bogus");
        assert!(format!("{}", err).contains("unknown field"));
    }

    #[test]
    fn reconcile_config_rejects_drift_policy_typo() {
        // Exactly the example called out in the v0.4 finding: `dirft_policy`
        // typo silently became a no-op; with deny_unknown_fields it must fail
        // loudly so the operator notices.
        let yaml = "interval: 5m\ndirftPolicy: Auto\n";
        let err = serde_yaml::from_str::<ReconcileConfig>(yaml)
            .expect_err("expected deny_unknown_fields to reject dirftPolicy");
        let msg = format!("{}", err);
        assert!(
            msg.contains("unknown field") && msg.contains("dirftPolicy"),
            "expected unknown-field error mentioning dirftPolicy, got: {msg}"
        );
    }

    #[test]
    fn policy_action_all_variants_deserialize() {
        let notify: PolicyAction = serde_yaml::from_str("Notify").unwrap();
        let accept: PolicyAction = serde_yaml::from_str("Accept").unwrap();
        let reject: PolicyAction = serde_yaml::from_str("Reject").unwrap();
        let ignore: PolicyAction = serde_yaml::from_str("Ignore").unwrap();
        assert_eq!(notify, PolicyAction::Notify);
        assert_eq!(accept, PolicyAction::Accept);
        assert_eq!(reject, PolicyAction::Reject);
        assert_eq!(ignore, PolicyAction::Ignore);
    }
}
