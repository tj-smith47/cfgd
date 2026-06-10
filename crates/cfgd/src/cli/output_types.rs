use std::collections::HashMap;

use serde::Serialize;

// --- Command output types ---

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogOutput {
    pub entries: Vec<cfgd_core::state::ApplyRecord>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct LogShowOutputOutput {
    pub apply_id: i64,
    pub entries: Vec<LogShowEntryOutput>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct LogShowEntryOutput {
    pub phase: String,
    pub resource_id: String,
    pub action_type: String,
    pub output: String,
}

/// Structured payload for `cfgd apply`. Carries the result of an apply run for
/// `-o json|yaml|jsonpath|template` consumers (CI, scripts, the operator). The
/// optional fields cover the no-op paths (`nothing_to_do`, `aborted`) where no
/// reconciler run happened and there is no apply_id to report.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyOutput {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apply_id: Option<i64>,
    pub succeeded: usize,
    pub failed: usize,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub source_commits: HashMap<String, String>,
}

impl ApplyOutput {
    pub fn nothing_to_do() -> Self {
        Self {
            status: "nothingToDo".to_string(),
            apply_id: None,
            succeeded: 0,
            failed: 0,
            source_commits: HashMap::new(),
        }
    }

    pub fn aborted() -> Self {
        Self {
            status: "aborted".to_string(),
            apply_id: None,
            succeeded: 0,
            failed: 0,
            source_commits: HashMap::new(),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RollbackOutput {
    pub apply_id: i64,
    pub files_restored: usize,
    pub files_removed: usize,
    pub non_file_actions: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncOutput {
    pub local_pulled: bool,
    pub sources: Vec<SourceSyncOutput>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PullOutput {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckinOutput {
    pub server_status: String,
    pub config_changed: bool,
    pub drift_count: usize,
    pub drift_status: String,
    pub server_pushed_config: bool,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffOutput {
    pub packages: Vec<PackageDrift>,
    pub system: Vec<SystemDriftOutput>,
    pub summary: DiffSummary,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffSummary {
    pub has_file_drift: bool,
    pub has_pkg_drift: bool,
    pub has_system_drift: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageDrift {
    pub manager: String,
    pub shape: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub packages: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bootstrap_method: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemDriftOutput {
    pub key: String,
    pub expected: String,
    pub actual: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceSyncOutput {
    pub name: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanOutput {
    pub context: String,
    pub phases: Vec<PlanPhaseOutput>,
    pub total_actions: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanPhaseOutput {
    pub phase: String,
    pub actions: Vec<PlanActionOutput>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanActionOutput {
    pub description: String,
    #[serde(rename = "type")]
    pub action_type: String,
    /// Absolute filesystem path(s) this action writes. Empty (and omitted from
    /// the wire) for actions with no direct filesystem target — package
    /// installs, system-configurator writes, live-session refresh. Lets `-o
    /// json` consumers (CI, blast-radius tooling) read the target without
    /// scraping `description`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<String>,
    /// The ConfigSource that delivered the resource's body, when not local.
    /// `Some(source_name)` for source-delivered modules/files/packages; omitted
    /// from the wire (and `None`) for consumer-local resources. Lets `-o json`
    /// consumers read provenance without scraping the ` <- <source>` suffix off
    /// `description`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorOutput {
    pub config: DoctorConfigCheck,
    pub git: bool,
    pub secrets: DoctorSecretsCheck,
    pub package_managers: Vec<DoctorManagerCheck>,
    pub modules: Vec<DoctorModuleCheck>,
    pub system_configurators: Vec<DoctorConfiguratorCheck>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorConfigCheck {
    pub valid: bool,
    pub path: String,
    pub name: Option<String>,
    pub profile: Option<String>,
    pub error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorSecretsCheck {
    pub sops_available: bool,
    pub sops_version: Option<String>,
    pub age_key_exists: bool,
    pub age_key_path: Option<String>,
    pub sops_config_exists: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sops_config_path: Option<String>,
    pub providers: Vec<DoctorProviderCheck>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorProviderCheck {
    pub name: String,
    pub available: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorManagerCheck {
    pub name: String,
    pub available: bool,
    pub declared: bool,
    pub can_bootstrap: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bootstrap_method: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorModuleCheck {
    pub name: String,
    pub valid: bool,
    pub error: Option<String>,
    #[serde(default)]
    pub packages: Vec<DoctorModulePackageCheck>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorModulePackageCheck {
    pub name: String,
    pub resolved_name: String,
    pub manager: String,
    pub installed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorConfiguratorCheck {
    pub name: String,
    pub available: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceListEntry {
    pub name: String,
    pub url: String,
    pub priority: u32,
    pub version: Option<String>,
    pub status: String,
    pub last_fetched: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceShowOutput {
    pub name: String,
    pub url: String,
    pub branch: String,
    pub priority: u32,
    pub accept_recommended: bool,
    pub profile: Option<String>,
    pub sync_interval: String,
    pub auto_apply: bool,
    pub pin_version: Option<String>,
    pub state: Option<SourceStateInfo>,
    pub managed_resources: Vec<SourceResourceEntry>,
    /// Module names this source declares deliverable — its manifest
    /// `spec.provides.modules` allow-list (the module bodies it offers to
    /// subscribers). Empty (and omitted from the wire) when the source delivers
    /// no modules or its manifest could not be loaded.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modules: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceStateInfo {
    pub status: String,
    pub last_fetched: Option<String>,
    pub last_commit: Option<String>,
    pub version: Option<String>,
    /// Resolved tag name from sources.lock (None for HEAD-tracking sources).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked_ref: Option<String>,
    /// 40-char commit SHA from sources.lock at time of last lock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked_commit: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceResourceEntry {
    pub resource_type: String,
    pub resource_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileListEntry {
    pub name: String,
    pub active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inherits: Option<String>,
    pub module_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct ModuleSearchResult {
    pub name: String,
    pub registry: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct RegistryListEntry {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AliasListEntry {
    pub name: String,
    pub command: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct KeyListEntry {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
}

// --- Compliance command output types ---

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct ComplianceSnapshotOutput {
    pub snapshot: cfgd_core::compliance::ComplianceSnapshot,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct ComplianceHistoryOutput {
    pub entries: Vec<cfgd_core::state::ComplianceHistoryRow>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct ComplianceDiffOutput {
    pub id1: i64,
    pub id2: i64,
    pub added: Vec<cfgd_core::compliance::ComplianceCheck>,
    pub removed: Vec<cfgd_core::compliance::ComplianceCheck>,
    pub changed: Vec<ComplianceCheckChange>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComplianceCheckChange {
    pub key: String,
    pub old_status: String,
    pub new_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use cfgd_core::compliance::{
        ComplianceCheck, ComplianceSnapshot, ComplianceStatus, ComplianceSummary, MachineInfo,
    };
    use cfgd_core::state::{ApplyRecord, ApplyStatus, ComplianceHistoryRow};
    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};

    #[test]
    fn log_output_serializes_entries_array_under_camelcase_key() {
        let v = LogOutput {
            entries: vec![ApplyRecord {
                id: 7,
                timestamp: "2026-01-02T03:04:05Z".to_string(),
                profile: "default".to_string(),
                plan_hash: "deadbeef".to_string(),
                status: ApplyStatus::Success,
                summary: Some("ok".to_string()),
            }],
        };
        let json = serde_json::to_value(&v).unwrap();
        let entries = json["entries"].as_array().expect("entries is array");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["id"], json!(7));
        assert_eq!(entries[0]["planHash"], json!("deadbeef"));
        assert_eq!(entries[0]["status"], json!("Success"));
    }

    #[test]
    fn log_show_output_output_pins_apply_id_and_nested_entries() {
        let v = LogShowOutputOutput {
            apply_id: 42,
            entries: vec![LogShowEntryOutput {
                phase: "pre".to_string(),
                resource_id: "res-1".to_string(),
                action_type: "install".to_string(),
                output: "ok\n".to_string(),
            }],
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["applyId"], json!(42));
        let entries = json["entries"].as_array().expect("entries is array");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["phase"], json!("pre"));
        assert_eq!(entries[0]["resourceId"], json!("res-1"));
        assert_eq!(entries[0]["actionType"], json!("install"));
        assert_eq!(entries[0]["output"], json!("ok\n"));
    }

    #[test]
    fn log_show_entry_output_uses_camelcase_for_every_field() {
        let v = LogShowEntryOutput {
            phase: "main".to_string(),
            resource_id: "res-2".to_string(),
            action_type: "configure".to_string(),
            output: "done".to_string(),
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["phase"], json!("main"));
        assert_eq!(json["resourceId"], json!("res-2"));
        assert_eq!(json["actionType"], json!("configure"));
        assert_eq!(json["output"], json!("done"));
    }

    #[test]
    fn apply_output_nothing_to_do_emits_sentinel_status_and_skips_empty_optionals() {
        let v = ApplyOutput::nothing_to_do();
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["status"], json!("nothingToDo"));
        assert_eq!(json["succeeded"], json!(0));
        assert_eq!(json["failed"], json!(0));
        assert!(
            json.get("applyId").is_none(),
            "applyId must be skipped when None"
        );
        assert!(
            json.get("sourceCommits").is_none(),
            "sourceCommits must be skipped when HashMap is empty"
        );
    }

    #[test]
    fn apply_output_aborted_emits_aborted_status_and_zero_counts() {
        let v = ApplyOutput::aborted();
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["status"], json!("aborted"));
        assert_eq!(json["succeeded"], json!(0));
        assert_eq!(json["failed"], json!(0));
        assert!(json.get("applyId").is_none());
        assert!(json.get("sourceCommits").is_none());
    }

    #[test]
    fn apply_output_populated_includes_apply_id_and_source_commits() {
        let mut commits = HashMap::new();
        commits.insert("origin".to_string(), "abc123".to_string());
        let v = ApplyOutput {
            status: "success".to_string(),
            apply_id: Some(99),
            succeeded: 3,
            failed: 1,
            source_commits: commits,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["status"], json!("success"));
        assert_eq!(json["applyId"], json!(99));
        assert_eq!(json["succeeded"], json!(3));
        assert_eq!(json["failed"], json!(1));
        assert_eq!(json["sourceCommits"]["origin"], json!("abc123"));
    }

    #[test]
    fn rollback_output_camelcases_all_fields_and_includes_action_list() {
        let v = RollbackOutput {
            apply_id: 12,
            files_restored: 4,
            files_removed: 2,
            non_file_actions: vec!["svc:restart".to_string()],
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["applyId"], json!(12));
        assert_eq!(json["filesRestored"], json!(4));
        assert_eq!(json["filesRemoved"], json!(2));
        assert_eq!(json["nonFileActions"], json!(["svc:restart"]));
    }

    #[test]
    fn sync_output_pins_local_pulled_flag_and_sources_array() {
        let v = SyncOutput {
            local_pulled: true,
            sources: vec![SourceSyncOutput {
                name: "main".to_string(),
                status: "synced".to_string(),
                commit: Some("c0ffee".to_string()),
            }],
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["localPulled"], json!(true));
        let sources = json["sources"].as_array().expect("sources is array");
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0]["name"], json!("main"));
        assert_eq!(sources[0]["status"], json!("synced"));
        assert_eq!(sources[0]["commit"], json!("c0ffee"));
    }

    #[test]
    fn pull_output_skips_error_when_none_and_keeps_status() {
        let v = PullOutput {
            status: "upToDate".to_string(),
            error: None,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["status"], json!("upToDate"));
        assert!(
            json.get("error").is_none(),
            "error must be skipped when None"
        );
    }

    #[test]
    fn pull_output_includes_error_when_some() {
        let v = PullOutput {
            status: "failed".to_string(),
            error: Some("network unreachable".to_string()),
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["status"], json!("failed"));
        assert_eq!(json["error"], json!("network unreachable"));
    }

    #[test]
    fn checkin_output_camelcases_all_fields() {
        let v = CheckinOutput {
            server_status: "ok".to_string(),
            config_changed: true,
            drift_count: 5,
            drift_status: "warning".to_string(),
            server_pushed_config: false,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["serverStatus"], json!("ok"));
        assert_eq!(json["configChanged"], json!(true));
        assert_eq!(json["driftCount"], json!(5));
        assert_eq!(json["driftStatus"], json!("warning"));
        assert_eq!(json["serverPushedConfig"], json!(false));
    }

    #[test]
    fn diff_output_nests_packages_system_and_summary() {
        let v = DiffOutput {
            packages: vec![PackageDrift {
                manager: "brew".to_string(),
                shape: "missing".to_string(),
                packages: vec!["ripgrep".to_string()],
                bootstrap_method: Some("script".to_string()),
            }],
            system: vec![SystemDriftOutput {
                key: "sysctl.kernel.x".to_string(),
                expected: "1".to_string(),
                actual: "0".to_string(),
            }],
            summary: DiffSummary {
                has_file_drift: true,
                has_pkg_drift: true,
                has_system_drift: true,
            },
        };
        let json = serde_json::to_value(&v).unwrap();
        let pkgs = json["packages"].as_array().expect("packages is array");
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0]["manager"], json!("brew"));
        assert_eq!(pkgs[0]["packages"], json!(["ripgrep"]));
        let sys = json["system"].as_array().expect("system is array");
        assert_eq!(sys.len(), 1);
        assert_eq!(sys[0]["key"], json!("sysctl.kernel.x"));
        assert_eq!(json["summary"]["hasFileDrift"], json!(true));
        assert_eq!(json["summary"]["hasPkgDrift"], json!(true));
        assert_eq!(json["summary"]["hasSystemDrift"], json!(true));
    }

    #[test]
    fn diff_summary_camelcases_drift_flags() {
        let v = DiffSummary {
            has_file_drift: false,
            has_pkg_drift: true,
            has_system_drift: false,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["hasFileDrift"], json!(false));
        assert_eq!(json["hasPkgDrift"], json!(true));
        assert_eq!(json["hasSystemDrift"], json!(false));
    }

    #[test]
    fn package_drift_skips_empty_packages_and_none_bootstrap() {
        let v = PackageDrift {
            manager: "apt".to_string(),
            shape: "extra".to_string(),
            packages: vec![],
            bootstrap_method: None,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["manager"], json!("apt"));
        assert_eq!(json["shape"], json!("extra"));
        assert!(
            json.get("packages").is_none(),
            "packages must be skipped when Vec is empty"
        );
        assert!(
            json.get("bootstrapMethod").is_none(),
            "bootstrapMethod must be skipped when None"
        );
    }

    #[test]
    fn package_drift_emits_packages_and_bootstrap_method_when_populated() {
        let v = PackageDrift {
            manager: "cargo".to_string(),
            shape: "missing".to_string(),
            packages: vec!["bat".to_string(), "fd-find".to_string()],
            bootstrap_method: Some("rustup".to_string()),
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["packages"], json!(["bat", "fd-find"]));
        assert_eq!(json["bootstrapMethod"], json!("rustup"));
    }

    #[test]
    fn system_drift_output_camelcases_fields() {
        let v = SystemDriftOutput {
            key: "kernel.parameter".to_string(),
            expected: "1024".to_string(),
            actual: "512".to_string(),
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["key"], json!("kernel.parameter"));
        assert_eq!(json["expected"], json!("1024"));
        assert_eq!(json["actual"], json!("512"));
    }

    #[test]
    fn source_sync_output_skips_none_commit() {
        let v = SourceSyncOutput {
            name: "infra".to_string(),
            status: "pending".to_string(),
            commit: None,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["name"], json!("infra"));
        assert_eq!(json["status"], json!("pending"));
        assert!(
            json.get("commit").is_none(),
            "commit must be skipped when None"
        );
    }

    #[test]
    fn plan_output_skips_empty_warnings_and_emits_phases() {
        let v = PlanOutput {
            context: "default".to_string(),
            phases: vec![PlanPhaseOutput {
                phase: "pre".to_string(),
                actions: vec![PlanActionOutput {
                    description: "install pkg".to_string(),
                    action_type: "package".to_string(),
                    targets: vec![],
                    origin: None,
                }],
            }],
            total_actions: 1,
            warnings: vec![],
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["context"], json!("default"));
        assert_eq!(json["totalActions"], json!(1));
        assert!(
            json.get("warnings").is_none(),
            "warnings must be skipped when Vec is empty"
        );
        let phases = json["phases"].as_array().expect("phases is array");
        assert_eq!(phases.len(), 1);
        assert_eq!(phases[0]["phase"], json!("pre"));
        let actions = phases[0]["actions"].as_array().expect("actions is array");
        assert_eq!(actions[0]["description"], json!("install pkg"));
    }

    #[test]
    fn plan_output_emits_warnings_when_populated() {
        let v = PlanOutput {
            context: "default".to_string(),
            phases: vec![],
            total_actions: 0,
            warnings: vec!["missing tool".to_string()],
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["warnings"], json!(["missing tool"]));
    }

    #[test]
    fn plan_phase_output_emits_phase_name_and_actions_array() {
        let v = PlanPhaseOutput {
            phase: "main".to_string(),
            actions: vec![PlanActionOutput {
                description: "render file".to_string(),
                action_type: "file".to_string(),
                targets: vec!["/etc/hosts".to_string()],
                origin: None,
            }],
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["phase"], json!("main"));
        let actions = json["actions"].as_array().expect("actions is array");
        assert_eq!(actions.len(), 1);
    }

    #[test]
    fn plan_action_output_renames_action_type_to_type() {
        let v = PlanActionOutput {
            description: "configure systemd".to_string(),
            action_type: "system".to_string(),
            targets: vec![],
            origin: None,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["description"], json!("configure systemd"));
        assert_eq!(
            json["type"],
            json!("system"),
            "action_type must rename to `type` on the wire"
        );
        assert!(
            json.get("targets").is_none(),
            "targets must be omitted from the wire when empty"
        );
        assert!(
            json.get("actionType").is_none(),
            "actionType camelCase must not appear; #[serde(rename)] takes precedence"
        );
    }

    #[test]
    fn plan_action_output_emits_targets_when_populated() {
        let v = PlanActionOutput {
            description: "create /etc/hosts".to_string(),
            action_type: "file.create".to_string(),
            targets: vec!["/etc/hosts".to_string()],
            origin: None,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(
            json["targets"],
            json!(["/etc/hosts"]),
            "targets must serialize as a string array when present"
        );
    }

    #[test]
    fn doctor_output_nests_all_subchecks() {
        let v = DoctorOutput {
            config: DoctorConfigCheck {
                valid: true,
                path: "/etc/cfgd.yaml".to_string(),
                name: Some("host".to_string()),
                profile: Some("default".to_string()),
                error: None,
            },
            git: true,
            secrets: DoctorSecretsCheck {
                sops_available: true,
                sops_version: Some("3.8.0".to_string()),
                age_key_exists: true,
                age_key_path: Some("/home/u/.age".to_string()),
                sops_config_exists: false,
                sops_config_path: None,
                providers: vec![DoctorProviderCheck {
                    name: "sops".to_string(),
                    available: true,
                }],
            },
            package_managers: vec![DoctorManagerCheck {
                name: "brew".to_string(),
                available: true,
                declared: true,
                can_bootstrap: false,
                bootstrap_method: None,
            }],
            modules: vec![DoctorModuleCheck {
                name: "shell".to_string(),
                valid: true,
                error: None,
                packages: vec![],
            }],
            system_configurators: vec![DoctorConfiguratorCheck {
                name: "systemd".to_string(),
                available: true,
            }],
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["config"]["valid"], json!(true));
        assert_eq!(json["git"], json!(true));
        assert_eq!(json["secrets"]["sopsAvailable"], json!(true));
        assert_eq!(json["packageManagers"][0]["name"], json!("brew"));
        assert_eq!(json["modules"][0]["name"], json!("shell"));
        assert_eq!(json["systemConfigurators"][0]["name"], json!("systemd"));
    }

    #[test]
    fn doctor_config_check_camelcases_fields_and_emits_nulls() {
        let v = DoctorConfigCheck {
            valid: false,
            path: "/x".to_string(),
            name: None,
            profile: None,
            error: Some("missing".to_string()),
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["valid"], json!(false));
        assert_eq!(json["path"], json!("/x"));
        assert_eq!(json["name"], Value::Null);
        assert_eq!(json["profile"], Value::Null);
        assert_eq!(json["error"], json!("missing"));
    }

    #[test]
    fn doctor_secrets_check_skips_sops_config_path_when_none() {
        let v = DoctorSecretsCheck {
            sops_available: true,
            sops_version: Some("3.8.0".to_string()),
            age_key_exists: false,
            age_key_path: None,
            sops_config_exists: false,
            sops_config_path: None,
            providers: vec![],
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["sopsAvailable"], json!(true));
        assert_eq!(json["sopsVersion"], json!("3.8.0"));
        assert_eq!(json["ageKeyExists"], json!(false));
        assert_eq!(json["ageKeyPath"], Value::Null);
        assert_eq!(json["sopsConfigExists"], json!(false));
        assert!(
            json.get("sopsConfigPath").is_none(),
            "sopsConfigPath must be skipped when None"
        );
        assert_eq!(json["providers"], json!([]));
    }

    #[test]
    fn doctor_secrets_check_includes_sops_config_path_when_some() {
        let v = DoctorSecretsCheck {
            sops_available: true,
            sops_version: None,
            age_key_exists: true,
            age_key_path: None,
            sops_config_exists: true,
            sops_config_path: Some("/etc/sops.yaml".to_string()),
            providers: vec![],
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["sopsConfigPath"], json!("/etc/sops.yaml"));
    }

    #[test]
    fn doctor_provider_check_emits_name_and_available() {
        let v = DoctorProviderCheck {
            name: "vault".to_string(),
            available: false,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["name"], json!("vault"));
        assert_eq!(json["available"], json!(false));
    }

    #[test]
    fn doctor_manager_check_skips_bootstrap_method_when_none() {
        let v = DoctorManagerCheck {
            name: "apt".to_string(),
            available: true,
            declared: false,
            can_bootstrap: false,
            bootstrap_method: None,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["name"], json!("apt"));
        assert_eq!(json["available"], json!(true));
        assert_eq!(json["declared"], json!(false));
        assert_eq!(json["canBootstrap"], json!(false));
        assert!(
            json.get("bootstrapMethod").is_none(),
            "bootstrapMethod must be skipped when None"
        );
    }

    #[test]
    fn doctor_manager_check_includes_bootstrap_method_when_some() {
        let v = DoctorManagerCheck {
            name: "brew".to_string(),
            available: false,
            declared: true,
            can_bootstrap: true,
            bootstrap_method: Some("curl-installer".to_string()),
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["bootstrapMethod"], json!("curl-installer"));
    }

    #[test]
    fn doctor_module_check_emits_packages_array_and_null_error() {
        let v = DoctorModuleCheck {
            name: "git".to_string(),
            valid: true,
            error: None,
            packages: vec![DoctorModulePackageCheck {
                name: "git".to_string(),
                resolved_name: "git".to_string(),
                manager: "apt".to_string(),
                installed: true,
                version: Some("2.40.1".to_string()),
                skip_reason: None,
                error: None,
            }],
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["name"], json!("git"));
        assert_eq!(json["valid"], json!(true));
        assert_eq!(json["error"], Value::Null);
        let pkgs = json["packages"].as_array().expect("packages is array");
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0]["resolvedName"], json!("git"));
    }

    #[test]
    fn doctor_module_package_check_skips_all_none_optionals() {
        let v = DoctorModulePackageCheck {
            name: "ripgrep".to_string(),
            resolved_name: "ripgrep".to_string(),
            manager: "brew".to_string(),
            installed: false,
            version: None,
            skip_reason: None,
            error: None,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["name"], json!("ripgrep"));
        assert_eq!(json["resolvedName"], json!("ripgrep"));
        assert_eq!(json["manager"], json!("brew"));
        assert_eq!(json["installed"], json!(false));
        assert!(json.get("version").is_none(), "version must be skipped");
        assert!(
            json.get("skipReason").is_none(),
            "skipReason must be skipped"
        );
        assert!(json.get("error").is_none(), "error must be skipped");
    }

    #[test]
    fn doctor_module_package_check_includes_all_optionals_when_populated() {
        let v = DoctorModulePackageCheck {
            name: "bat".to_string(),
            resolved_name: "bat-cat".to_string(),
            manager: "cargo".to_string(),
            installed: false,
            version: Some("0.24.0".to_string()),
            skip_reason: Some("offline".to_string()),
            error: Some("network".to_string()),
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["version"], json!("0.24.0"));
        assert_eq!(json["skipReason"], json!("offline"));
        assert_eq!(json["error"], json!("network"));
    }

    #[test]
    fn doctor_configurator_check_emits_name_and_available() {
        let v = DoctorConfiguratorCheck {
            name: "launchd".to_string(),
            available: true,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["name"], json!("launchd"));
        assert_eq!(json["available"], json!(true));
    }

    #[test]
    fn source_list_entry_camelcases_last_fetched_and_emits_nulls() {
        let v = SourceListEntry {
            name: "main".to_string(),
            url: "https://example.com/repo.git".to_string(),
            priority: 100,
            version: None,
            status: "synced".to_string(),
            last_fetched: Some("2026-01-01T00:00:00Z".to_string()),
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["name"], json!("main"));
        assert_eq!(json["url"], json!("https://example.com/repo.git"));
        assert_eq!(json["priority"], json!(100));
        assert_eq!(json["version"], Value::Null);
        assert_eq!(json["status"], json!("synced"));
        assert_eq!(json["lastFetched"], json!("2026-01-01T00:00:00Z"));
    }

    #[test]
    fn source_show_output_camelcases_all_fields_and_nests_state() {
        let v = SourceShowOutput {
            name: "infra".to_string(),
            url: "https://example.com/r.git".to_string(),
            branch: "main".to_string(),
            priority: 50,
            accept_recommended: true,
            profile: Some("default".to_string()),
            sync_interval: "5m".to_string(),
            auto_apply: false,
            pin_version: Some("v1.2.3".to_string()),
            state: Some(SourceStateInfo {
                status: "fresh".to_string(),
                last_fetched: Some("2026-01-01T00:00:00Z".to_string()),
                last_commit: Some("abc".to_string()),
                version: Some("v1.2.3".to_string()),
                locked_ref: None,
                locked_commit: None,
            }),
            managed_resources: vec![SourceResourceEntry {
                resource_type: "Module".to_string(),
                resource_id: "shell".to_string(),
            }],
            modules: vec!["dev-tools".to_string()],
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["modules"], json!(["dev-tools"]));
        assert_eq!(json["name"], json!("infra"));
        assert_eq!(json["url"], json!("https://example.com/r.git"));
        assert_eq!(json["branch"], json!("main"));
        assert_eq!(json["priority"], json!(50));
        assert_eq!(json["acceptRecommended"], json!(true));
        assert_eq!(json["profile"], json!("default"));
        assert_eq!(json["syncInterval"], json!("5m"));
        assert_eq!(json["autoApply"], json!(false));
        assert_eq!(json["pinVersion"], json!("v1.2.3"));
        assert_eq!(json["state"]["status"], json!("fresh"));
        assert_eq!(json["managedResources"][0]["resourceType"], json!("Module"));
    }

    #[test]
    fn source_state_info_emits_camelcase_keys() {
        let v = SourceStateInfo {
            status: "stale".to_string(),
            last_fetched: Some("2026-01-01T00:00:00Z".to_string()),
            last_commit: Some("c0ffee".to_string()),
            version: Some("v0.1".to_string()),
            locked_ref: None,
            locked_commit: None,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["status"], json!("stale"));
        assert_eq!(json["lastFetched"], json!("2026-01-01T00:00:00Z"));
        assert_eq!(json["lastCommit"], json!("c0ffee"));
        assert_eq!(json["version"], json!("v0.1"));
    }

    #[test]
    fn source_resource_entry_camelcases_resource_type_and_id() {
        let v = SourceResourceEntry {
            resource_type: "Profile".to_string(),
            resource_id: "dev".to_string(),
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["resourceType"], json!("Profile"));
        assert_eq!(json["resourceId"], json!("dev"));
    }

    #[test]
    fn profile_list_entry_skips_none_inherits_and_emits_module_count() {
        let v = ProfileListEntry {
            name: "default".to_string(),
            active: true,
            inherits: None,
            module_count: 3,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["name"], json!("default"));
        assert_eq!(json["active"], json!(true));
        assert_eq!(json["moduleCount"], json!(3));
        assert!(
            json.get("inherits").is_none(),
            "inherits must be skipped when None"
        );
    }

    #[test]
    fn profile_list_entry_includes_inherits_when_some() {
        let v = ProfileListEntry {
            name: "dev".to_string(),
            active: false,
            inherits: Some("base".to_string()),
            module_count: 5,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["inherits"], json!("base"));
    }

    #[test]
    fn module_search_result_skips_none_description_and_version() {
        let v = ModuleSearchResult {
            name: "shell".to_string(),
            registry: "official".to_string(),
            description: None,
            version: None,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["name"], json!("shell"));
        assert_eq!(json["registry"], json!("official"));
        assert!(json.get("description").is_none());
        assert!(json.get("version").is_none());
    }

    #[test]
    fn module_search_result_includes_description_and_version_when_some() {
        let v = ModuleSearchResult {
            name: "git".to_string(),
            registry: "official".to_string(),
            description: Some("Git tooling".to_string()),
            version: Some("1.0.0".to_string()),
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["description"], json!("Git tooling"));
        assert_eq!(json["version"], json!("1.0.0"));
    }

    #[test]
    fn registry_list_entry_emits_name_and_url() {
        let v = RegistryListEntry {
            name: "official".to_string(),
            url: "oci://registry.example.com".to_string(),
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["name"], json!("official"));
        assert_eq!(json["url"], json!("oci://registry.example.com"));
    }

    #[test]
    fn key_list_entry_skips_none_fingerprint_and_created() {
        let v = KeyListEntry {
            name: "signing".to_string(),
            fingerprint: None,
            created: None,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["name"], json!("signing"));
        assert!(json.get("fingerprint").is_none());
        assert!(json.get("created").is_none());
    }

    #[test]
    fn key_list_entry_includes_fingerprint_and_created_when_some() {
        let v = KeyListEntry {
            name: "signing".to_string(),
            fingerprint: Some("SHA256:abc".to_string()),
            created: Some("2026-01-01T00:00:00Z".to_string()),
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["fingerprint"], json!("SHA256:abc"));
        assert_eq!(json["created"], json!("2026-01-01T00:00:00Z"));
    }

    fn sample_snapshot() -> ComplianceSnapshot {
        ComplianceSnapshot {
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            machine: MachineInfo {
                hostname: "host".to_string(),
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
            },
            profile: "default".to_string(),
            sources: vec!["main".to_string()],
            checks: vec![],
            summary: ComplianceSummary {
                compliant: 1,
                warning: 0,
                violation: 0,
            },
        }
    }

    #[test]
    fn compliance_snapshot_output_nests_snapshot_payload() {
        let v = ComplianceSnapshotOutput {
            snapshot: sample_snapshot(),
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["snapshot"]["profile"], json!("default"));
        assert_eq!(json["snapshot"]["summary"]["compliant"], json!(1));
    }

    #[test]
    fn compliance_history_output_emits_entries_array() {
        let v = ComplianceHistoryOutput {
            entries: vec![ComplianceHistoryRow {
                id: 1,
                timestamp: "2026-01-01T00:00:00Z".to_string(),
                compliant: 10,
                warning: 2,
                violation: 1,
            }],
        };
        let json = serde_json::to_value(&v).unwrap();
        let entries = json["entries"].as_array().expect("entries is array");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["id"], json!(1));
        assert_eq!(entries[0]["compliant"], json!(10));
    }

    #[test]
    fn compliance_diff_output_camelcases_id_fields_and_nests_changes() {
        let v = ComplianceDiffOutput {
            id1: 1,
            id2: 2,
            added: vec![ComplianceCheck {
                category: "pkg".to_string(),
                status: ComplianceStatus::Compliant,
                ..ComplianceCheck::default()
            }],
            removed: vec![],
            changed: vec![ComplianceCheckChange {
                key: "pkg/git".to_string(),
                old_status: "Compliant".to_string(),
                new_status: "Violation".to_string(),
                detail: Some("missing".to_string()),
            }],
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["id1"], json!(1));
        assert_eq!(json["id2"], json!(2));
        let added = json["added"].as_array().expect("added is array");
        assert_eq!(added.len(), 1);
        assert_eq!(added[0]["category"], json!("pkg"));
        assert_eq!(json["removed"], json!([]));
        let changed = json["changed"].as_array().expect("changed is array");
        assert_eq!(changed[0]["key"], json!("pkg/git"));
        assert_eq!(changed[0]["oldStatus"], json!("Compliant"));
        assert_eq!(changed[0]["newStatus"], json!("Violation"));
        assert_eq!(changed[0]["detail"], json!("missing"));
    }

    #[test]
    fn compliance_check_change_skips_none_detail() {
        let v = ComplianceCheckChange {
            key: "pkg/x".to_string(),
            old_status: "Warning".to_string(),
            new_status: "Compliant".to_string(),
            detail: None,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["key"], json!("pkg/x"));
        assert_eq!(json["oldStatus"], json!("Warning"));
        assert_eq!(json["newStatus"], json!("Compliant"));
        assert!(
            json.get("detail").is_none(),
            "detail must be skipped when None"
        );
    }
}
