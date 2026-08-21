use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::source::default_sync_interval;

/// `spec.sync`: automatic push/pull settings for the daemon's git sync loop.
///
/// ```yaml
/// sync:
///   autoPush: true
///   autoPull: true
///   interval: 5m
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SyncConfig {
    /// Automatically commit and push local config changes. Default: `false`.
    #[serde(default)]
    pub auto_push: bool,
    /// Automatically pull upstream config changes. Default: `false`.
    #[serde(default)]
    pub auto_pull: bool,
    /// How often the daemon runs the sync loop, as a duration string (`"5m"`, `"1h"`).
    /// Default: `1h`.
    #[serde(default = "default_sync_interval")]
    pub interval: String,
}

/// `spec.notify`: how the daemon reports detected drift.
///
/// ```yaml
/// notify:
///   drift: true
///   method: Webhook
///   webhookUrl: https://example.com/hook
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NotifyConfig {
    /// Send a notification whenever the daemon detects drift. Default: `false`.
    #[serde(default)]
    pub drift: bool,
    /// Where a drift notification is delivered. Default: `Desktop`.
    #[serde(default)]
    pub method: NotifyMethod,
    /// Webhook URL to POST to when `method` is `Webhook`. Required only for that method.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_url: Option<String>,
}

/// Delivery method for a `notify.method` value: `Desktop`, `Stdout`, or `Webhook`
/// (case-insensitive on read; case-sensitive `PascalCase` in generated YAML).
#[derive(Debug, Clone, Default, Serialize, schemars::JsonSchema)]
pub enum NotifyMethod {
    /// A native desktop notification.
    #[default]
    Desktop,
    /// A line written to the daemon's own stdout/log.
    Stdout,
    /// A JSON POST to `webhookUrl`.
    Webhook,
}

case_insensitive_enum!(NotifyMethod {
    "Desktop" => NotifyMethod::Desktop,
    "Stdout" => NotifyMethod::Stdout,
    "Webhook" => NotifyMethod::Webhook,
});

/// `spec.secrets`: the default secret backend and its integrations.
///
/// ```yaml
/// secrets:
///   backend: sops
///   sops:
///     ageKey: ~/.config/sops/age/keys.txt
///   integrations:
///     - name: 1password
///       vault: Personal
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SecretsConfig {
    /// Default secret backend name (`sops`, `1password`, `bitwarden`, `vault`, `age`).
    /// Default: `sops`.
    #[serde(default = "default_secrets_backend")]
    pub backend: String,
    /// sops-specific settings, used when `backend` is `sops`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sops: Option<SopsConfig>,
    /// Named backend integrations a `${secret:<name>:<ref>}` reference can select.
    #[serde(default)]
    pub integrations: Vec<SecretIntegration>,
}

fn default_secrets_backend() -> String {
    "sops".to_string()
}

/// sops backend settings under `spec.secrets.sops`.
///
/// ```yaml
/// sops:
///   ageKey: ~/.config/sops/age/keys.txt
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SopsConfig {
    /// Path to the age private key file sops decrypts with. Falls back to sops's
    /// own default search path when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age_key: Option<PathBuf>,
}

/// One named secret backend integration under `spec.secrets.integrations[]`.
///
/// Every field beyond `name` is backend-specific and captured verbatim into
/// `extra`, so it accepts arbitrary per-backend keys (`vault`/`item` for
/// 1Password, `mount`/`path` for Vault, and so on).
///
/// ```yaml
/// integrations:
///   - name: 1password
///     vault: Personal
///     item: GitHub Token
/// ```
// no deny_unknown_fields — incompatible with serde(flatten) on `extra`; the
// flattened map intentionally captures arbitrary per-backend keys (vault, item,
// etc.) and `deny_unknown_fields` would short-circuit that routing.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SecretIntegration {
    /// Integration name (`1password`, `bitwarden`, `vault`), referenced by
    /// `${secret:<name>:<ref>}`.
    pub name: String,
    /// Backend-specific settings, flattened to the top level in YAML.
    #[serde(flatten)]
    #[schemars(with = "std::collections::HashMap<String, serde_json::Value>")]
    pub extra: HashMap<String, serde_yaml::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::source::default_sync_interval;

    #[test]
    fn sync_config_defaults_auto_push_to_false_when_omitted() {
        let parsed: SyncConfig = serde_yaml::from_str("interval: 1h").unwrap();
        assert!(!parsed.auto_push);
        assert!(!parsed.auto_pull);
        assert_eq!(parsed.interval, "1h");
    }

    #[test]
    fn sync_config_uses_default_interval_when_all_omitted() {
        let parsed: SyncConfig = serde_yaml::from_str("{}").unwrap();
        assert_eq!(parsed.interval, default_sync_interval());
    }

    #[test]
    fn sync_config_yaml_uses_camelcase_field_names() {
        let v = SyncConfig {
            auto_push: true,
            auto_pull: false,
            interval: "5m".into(),
        };
        let yaml = serde_yaml::to_string(&v).unwrap();
        assert!(
            yaml.contains("autoPush: true"),
            "yaml missing autoPush: {yaml}"
        );
        assert!(
            yaml.contains("autoPull: false"),
            "yaml missing autoPull: {yaml}"
        );
        assert!(
            yaml.contains("interval: 5m"),
            "yaml missing interval: {yaml}"
        );
        assert!(
            !yaml.contains("auto_push"),
            "yaml leaked snake_case: {yaml}"
        );
        assert!(
            !yaml.contains("auto_pull"),
            "yaml leaked snake_case: {yaml}"
        );

        let parsed: SyncConfig = serde_yaml::from_str(&yaml).unwrap();
        assert!(parsed.auto_push);
        assert!(!parsed.auto_pull);
        assert_eq!(parsed.interval, "5m");
    }

    #[test]
    fn notify_config_defaults_drift_to_false() {
        let parsed: NotifyConfig = serde_yaml::from_str("{}").unwrap();
        assert!(!parsed.drift);
    }

    #[test]
    fn notify_config_defaults_method_to_desktop() {
        let parsed: NotifyConfig = serde_yaml::from_str("{}").unwrap();
        assert!(matches!(parsed.method, NotifyMethod::Desktop));
    }

    #[test]
    fn notify_config_omits_webhook_url() {
        let parsed: NotifyConfig = serde_yaml::from_str("{}").unwrap();
        assert!(parsed.webhook_url.is_none());

        let v = NotifyConfig {
            drift: false,
            method: NotifyMethod::Desktop,
            webhook_url: None,
        };
        let yaml = serde_yaml::to_string(&v).unwrap();
        assert!(
            !yaml.contains("webhookUrl"),
            "yaml should skip None webhookUrl: {yaml}"
        );
        assert!(
            !yaml.contains("webhook_url"),
            "yaml should skip None webhook_url: {yaml}"
        );
    }

    #[test]
    fn notify_config_yaml_uses_camelcase_field_names() {
        let v = NotifyConfig {
            drift: true,
            method: NotifyMethod::Webhook,
            webhook_url: Some("https://example.com/hook".into()),
        };
        let yaml = serde_yaml::to_string(&v).unwrap();
        assert!(yaml.contains("drift: true"), "yaml missing drift: {yaml}");
        assert!(
            yaml.contains("method: Webhook"),
            "yaml missing method: {yaml}"
        );
        assert!(
            yaml.contains("webhookUrl: https://example.com/hook"),
            "yaml missing webhookUrl: {yaml}"
        );
        assert!(
            !yaml.contains("webhook_url"),
            "yaml leaked snake_case: {yaml}"
        );

        let parsed: NotifyConfig = serde_yaml::from_str(&yaml).unwrap();
        assert!(parsed.drift);
        assert!(matches!(parsed.method, NotifyMethod::Webhook));
        assert_eq!(
            parsed.webhook_url.as_deref(),
            Some("https://example.com/hook")
        );
    }

    #[test]
    fn notify_method_default_is_desktop() {
        assert!(matches!(NotifyMethod::default(), NotifyMethod::Desktop));
    }

    #[test]
    fn notify_method_serializes_as_pascalcase_default() {
        let yaml = serde_yaml::to_string(&NotifyMethod::Desktop).unwrap();
        assert_eq!(yaml.trim(), "Desktop");
    }

    #[test]
    fn notify_method_deserializes_each_variant() {
        let cases = [
            ("Desktop", NotifyMethod::Desktop),
            ("Stdout", NotifyMethod::Stdout),
            ("Webhook", NotifyMethod::Webhook),
        ];
        for (input, expected) in cases {
            let parsed: NotifyMethod = serde_yaml::from_str(input).unwrap();
            match (&parsed, &expected) {
                (NotifyMethod::Desktop, NotifyMethod::Desktop)
                | (NotifyMethod::Stdout, NotifyMethod::Stdout)
                | (NotifyMethod::Webhook, NotifyMethod::Webhook) => {}
                _ => panic!("expected {expected:?} for input {input}, got {parsed:?}"),
            }
        }
    }

    #[test]
    fn notify_method_parses_case_insensitively() {
        for (token, expected) in [
            ("desktop", NotifyMethod::Desktop),
            ("DESKTOP", NotifyMethod::Desktop),
            ("Desktop", NotifyMethod::Desktop),
            ("stdout", NotifyMethod::Stdout),
            ("StdOut", NotifyMethod::Stdout),
            ("webhook", NotifyMethod::Webhook),
            ("WEBHOOK", NotifyMethod::Webhook),
        ] {
            let parsed: NotifyMethod = serde_yaml::from_str(token)
                .unwrap_or_else(|e| panic!("`{token}` should parse: {e}"));
            match (&parsed, &expected) {
                (NotifyMethod::Desktop, NotifyMethod::Desktop)
                | (NotifyMethod::Stdout, NotifyMethod::Stdout)
                | (NotifyMethod::Webhook, NotifyMethod::Webhook) => {}
                _ => panic!("token {token}: expected {expected:?}, got {parsed:?}"),
            }
        }
    }

    #[test]
    fn notify_method_rejects_garbage() {
        serde_yaml::from_str::<NotifyMethod>("email").expect_err("unknown NotifyMethod must error");
    }

    #[test]
    fn secrets_config_defaults_backend_to_sops() {
        let parsed: SecretsConfig = serde_yaml::from_str("{}").unwrap();
        assert_eq!(parsed.backend, "sops");
    }

    #[test]
    fn secrets_config_defaults_integrations_to_empty() {
        let parsed: SecretsConfig = serde_yaml::from_str("{}").unwrap();
        assert!(parsed.integrations.is_empty());
        assert!(parsed.sops.is_none());
    }

    #[test]
    fn sops_config_yaml_uses_camelcase_field_names() {
        let v = SopsConfig {
            age_key: Some(PathBuf::from("/etc/cfgd/age.key")),
        };
        let yaml = serde_yaml::to_string(&v).unwrap();
        assert!(
            yaml.contains("ageKey: /etc/cfgd/age.key"),
            "yaml missing ageKey: {yaml}"
        );
        assert!(!yaml.contains("age_key"), "yaml leaked snake_case: {yaml}");

        let parsed: SopsConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(
            parsed.age_key.as_deref(),
            Some(PathBuf::from("/etc/cfgd/age.key").as_path())
        );
    }

    #[test]
    fn sops_config_omits_age_key_when_none() {
        let v = SopsConfig { age_key: None };
        let yaml = serde_yaml::to_string(&v).unwrap();
        assert!(
            !yaml.contains("ageKey"),
            "yaml should skip None ageKey: {yaml}"
        );
        assert!(
            !yaml.contains("age_key"),
            "yaml should skip None age_key: {yaml}"
        );
    }

    #[test]
    fn secret_integration_flattens_extra_fields_into_top_level_yaml() {
        let mut extra: HashMap<String, serde_yaml::Value> = HashMap::new();
        extra.insert(
            "vault".to_string(),
            serde_yaml::Value::String("Personal".to_string()),
        );
        extra.insert(
            "item".to_string(),
            serde_yaml::Value::String("GitHub Token".to_string()),
        );
        let v = SecretIntegration {
            name: "1password".to_string(),
            extra,
        };

        let yaml = serde_yaml::to_string(&v).unwrap();
        assert!(
            yaml.contains("name: 1password"),
            "yaml missing name: {yaml}"
        );
        assert!(
            yaml.contains("vault: Personal"),
            "yaml missing vault: {yaml}"
        );
        assert!(
            yaml.contains("item: GitHub Token"),
            "yaml missing item: {yaml}"
        );
        assert!(
            !yaml.contains("extra:"),
            "yaml should not have nested extra block: {yaml}"
        );

        let parsed: SecretIntegration = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed.name, "1password");
        assert_eq!(parsed.extra.len(), 2);
        assert_eq!(
            parsed.extra.get("vault").and_then(|v| v.as_str()),
            Some("Personal")
        );
        assert_eq!(
            parsed.extra.get("item").and_then(|v| v.as_str()),
            Some("GitHub Token")
        );
    }

    #[test]
    fn secret_integration_collects_unknown_fields_into_extra() {
        let yaml = "name: foo\narbitrary: bar\nanother: 123\n";
        let parsed: SecretIntegration = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(parsed.name, "foo");
        assert_eq!(parsed.extra.len(), 2);
        assert_eq!(
            parsed.extra.get("arbitrary").and_then(|v| v.as_str()),
            Some("bar")
        );
        assert_eq!(
            parsed.extra.get("another").and_then(|v| v.as_i64()),
            Some(123)
        );
    }
}
