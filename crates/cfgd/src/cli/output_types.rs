use serde::Serialize;

// --- Command output types ---

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct LogOutput {
    pub(in crate::cli) entries: Vec<cfgd_core::state::ApplyRecord>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct RollbackOutput {
    pub(in crate::cli) apply_id: i64,
    pub(in crate::cli) files_restored: usize,
    pub(in crate::cli) files_removed: usize,
    pub(in crate::cli) non_file_actions: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct PlanOutput {
    pub(in crate::cli) context: String,
    pub(in crate::cli) phases: Vec<PlanPhaseOutput>,
    pub(in crate::cli) total_actions: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(in crate::cli) warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct PlanPhaseOutput {
    pub(in crate::cli) phase: String,
    pub(in crate::cli) actions: Vec<PlanActionOutput>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct PlanActionOutput {
    pub(in crate::cli) description: String,
    #[serde(rename = "type")]
    pub(in crate::cli) action_type: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct DoctorOutput {
    pub(in crate::cli) config: DoctorConfigCheck,
    pub(in crate::cli) git: bool,
    pub(in crate::cli) secrets: DoctorSecretsCheck,
    pub(in crate::cli) package_managers: Vec<DoctorManagerCheck>,
    pub(in crate::cli) modules: Vec<DoctorModuleCheck>,
    pub(in crate::cli) system_configurators: Vec<DoctorConfiguratorCheck>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct DoctorConfigCheck {
    pub(in crate::cli) valid: bool,
    pub(in crate::cli) path: String,
    pub(in crate::cli) name: Option<String>,
    pub(in crate::cli) profile: Option<String>,
    pub(in crate::cli) error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct DoctorSecretsCheck {
    pub(in crate::cli) sops_available: bool,
    pub(in crate::cli) sops_version: Option<String>,
    pub(in crate::cli) age_key_exists: bool,
    pub(in crate::cli) age_key_path: Option<String>,
    pub(in crate::cli) sops_config_exists: bool,
    pub(in crate::cli) providers: Vec<DoctorProviderCheck>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct DoctorProviderCheck {
    pub(in crate::cli) name: String,
    pub(in crate::cli) available: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct DoctorManagerCheck {
    pub(in crate::cli) name: String,
    pub(in crate::cli) available: bool,
    pub(in crate::cli) declared: bool,
    pub(in crate::cli) can_bootstrap: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct DoctorModuleCheck {
    pub(in crate::cli) name: String,
    pub(in crate::cli) valid: bool,
    pub(in crate::cli) error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct DoctorConfiguratorCheck {
    pub(in crate::cli) name: String,
    pub(in crate::cli) available: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct SourceListEntry {
    pub(in crate::cli) name: String,
    pub(in crate::cli) url: String,
    pub(in crate::cli) priority: u32,
    pub(in crate::cli) version: Option<String>,
    pub(in crate::cli) status: String,
    pub(in crate::cli) last_fetched: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct SourceShowOutput {
    pub(in crate::cli) name: String,
    pub(in crate::cli) url: String,
    pub(in crate::cli) branch: String,
    pub(in crate::cli) priority: u32,
    pub(in crate::cli) accept_recommended: bool,
    pub(in crate::cli) profile: Option<String>,
    pub(in crate::cli) sync_interval: String,
    pub(in crate::cli) auto_apply: bool,
    pub(in crate::cli) version_pin: Option<String>,
    pub(in crate::cli) state: Option<SourceStateInfo>,
    pub(in crate::cli) managed_resources: Vec<SourceResourceEntry>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct SourceStateInfo {
    pub(in crate::cli) status: String,
    pub(in crate::cli) last_fetched: Option<String>,
    pub(in crate::cli) last_commit: Option<String>,
    pub(in crate::cli) version: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct SourceResourceEntry {
    pub(in crate::cli) resource_type: String,
    pub(in crate::cli) resource_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct ProfileListEntry {
    pub(in crate::cli) name: String,
    pub(in crate::cli) active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(in crate::cli) inherits: Option<String>,
    pub(in crate::cli) module_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct ModuleSearchResult {
    pub(in crate::cli) name: String,
    pub(in crate::cli) registry: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(in crate::cli) description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(in crate::cli) version: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct RegistryListEntry {
    pub(in crate::cli) name: String,
    pub(in crate::cli) url: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct KeyListEntry {
    pub(in crate::cli) name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(in crate::cli) fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(in crate::cli) created: Option<String>,
}

// --- Compliance command output types ---

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct ComplianceSnapshotOutput {
    pub(in crate::cli) snapshot: cfgd_core::compliance::ComplianceSnapshot,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct ComplianceHistoryOutput {
    pub(in crate::cli) entries: Vec<cfgd_core::state::ComplianceHistoryRow>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct ComplianceDiffOutput {
    pub(in crate::cli) id1: i64,
    pub(in crate::cli) id2: i64,
    pub(in crate::cli) added: Vec<cfgd_core::compliance::ComplianceCheck>,
    pub(in crate::cli) removed: Vec<cfgd_core::compliance::ComplianceCheck>,
    pub(in crate::cli) changed: Vec<ComplianceCheckChange>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct ComplianceCheckChange {
    pub(in crate::cli) key: String,
    pub(in crate::cli) old_status: String,
    pub(in crate::cli) new_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(in crate::cli) detail: Option<String>,
}
