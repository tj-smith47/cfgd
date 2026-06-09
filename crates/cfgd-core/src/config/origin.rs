use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OriginSpec {
    #[serde(rename = "type")]
    pub origin_type: OriginType,
    pub url: String,
    #[serde(default = "default_branch")]
    pub branch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<String>,
    /// SSH `StrictHostKeyChecking` policy for git operations.
    /// `AcceptNew` (default): accept first-seen keys, reject changed keys.
    /// `Yes`: require keys to already exist in known_hosts (high-security).
    /// `No`: accept any key (insecure, not recommended).
    #[serde(default)]
    pub ssh_strict_host_key_checking: SshHostKeyPolicy,
}

/// SSH `StrictHostKeyChecking` policy for git operations over SSH.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, schemars::JsonSchema,
)]
pub enum SshHostKeyPolicy {
    /// Accept first-seen keys, reject changed keys (safe default for automation).
    #[default]
    AcceptNew,
    /// Require keys to already exist in known_hosts (high-security environments).
    Yes,
    /// Accept any key without verification (insecure, not recommended).
    No,
}

impl SshHostKeyPolicy {
    pub fn as_ssh_option(&self) -> &'static str {
        match self {
            SshHostKeyPolicy::AcceptNew => "accept-new",
            SshHostKeyPolicy::Yes => "yes",
            SshHostKeyPolicy::No => "no",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub enum OriginType {
    Git,
    Server,
}

fn default_branch() -> String {
    "master".to_string()
}
