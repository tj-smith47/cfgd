use serde::Serialize;

// --- Command output types ---

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogOutput {
    pub entries: Vec<cfgd_core::state::ApplyRecord>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct RollbackOutput {
    pub apply_id: i64,
    pub files_restored: usize,
    pub files_removed: usize,
    pub non_file_actions: Vec<String>,
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
pub(in crate::cli) struct SourceListEntry {
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
    pub version_pin: Option<String>,
    pub state: Option<SourceStateInfo>,
    pub managed_resources: Vec<SourceResourceEntry>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceStateInfo {
    pub status: String,
    pub last_fetched: Option<String>,
    pub last_commit: Option<String>,
    pub version: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceResourceEntry {
    pub resource_type: String,
    pub resource_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct ProfileListEntry {
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
