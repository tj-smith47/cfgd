use serde::{Deserialize, Serialize};

/// One entry of `spec.origin[]`: a remote this machine's config can sync with.
///
/// ```yaml
/// origin:
///   - type: Git
///     url: git@github.com:me/dotfiles.git
///     branch: main
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OriginSpec {
    /// Kind of origin: `Git` (a git remote) or `Server` (the device gateway).
    #[serde(rename = "type")]
    pub origin_type: OriginType,
    /// The origin's URL (a git remote, or the gateway's base URL).
    pub url: String,
    /// Branch to sync against. Default: `master`.
    #[serde(default = "default_branch")]
    pub branch: String,
    /// Auth method override for this origin (e.g. a credential-helper name).
    /// Omitted uses the ambient git/SSH credential configuration.
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

#[cfg(test)]
mod ssh_host_key_policy_tests {
    use super::SshHostKeyPolicy;

    #[test]
    fn as_ssh_option_maps_every_policy() {
        assert_eq!(SshHostKeyPolicy::AcceptNew.as_ssh_option(), "accept-new");
        assert_eq!(SshHostKeyPolicy::Yes.as_ssh_option(), "yes");
        assert_eq!(SshHostKeyPolicy::No.as_ssh_option(), "no");
    }
}

/// Kind of an origin: `Git` (a plain git remote) or `Server` (the device
/// gateway's enrollment/checkin API).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub enum OriginType {
    Git,
    Server,
}

fn default_branch() -> String {
    "master".to_string()
}
