use super::constants::GIT_NETWORK_TIMEOUT;
use super::paths::home_dir_var;
use super::process::{command_output_with_timeout, stderr_lossy_trimmed, stdout_lossy_trimmed};
use crate::config;

/// Prepare a `git` CLI command with SSH hang protection.
///
/// Sets `GIT_TERMINAL_PROMPT=0` to prevent interactive prompts and, for SSH URLs,
/// sets `GIT_SSH_COMMAND` with `BatchMode=yes` and configurable `StrictHostKeyChecking`
/// to prevent hangs in non-interactive contexts (piped install scripts, daemons).
///
/// The `ssh_policy` parameter controls the `StrictHostKeyChecking` value:
/// - `None` uses the default (`accept-new`)
/// - `Some(policy)` uses the specified policy
pub fn git_cmd_safe(
    url: Option<&str>,
    ssh_policy: Option<config::SshHostKeyPolicy>,
) -> std::process::Command {
    let mut cmd = std::process::Command::new("git");
    cmd.env("GIT_TERMINAL_PROMPT", "0")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    if url.is_some_and(|u| u.starts_with("git@") || u.starts_with("ssh://")) {
        let policy = ssh_policy.unwrap_or_default();
        cmd.env(
            "GIT_SSH_COMMAND",
            format!(
                "ssh -o BatchMode=yes -o StrictHostKeyChecking={}",
                policy.as_ssh_option()
            ),
        );
    }
    cmd
}

/// Try a git CLI command via [`git_cmd_safe`], returning `true` on success.
/// On failure, logs the stderr via `tracing::debug` and returns `false`.
pub fn try_git_cmd(
    url: Option<&str>,
    args: &[&str],
    label: &str,
    ssh_policy: Option<config::SshHostKeyPolicy>,
) -> bool {
    let mut cmd = git_cmd_safe(url, ssh_policy);
    cmd.args(args);
    match command_output_with_timeout(&mut cmd, GIT_NETWORK_TIMEOUT) {
        Ok(output) if output.status.success() => true,
        Ok(output) => {
            tracing::debug!(
                "git {} CLI failed (exit {}): {}",
                label,
                output.status.code().unwrap_or(-1),
                stderr_lossy_trimmed(&output),
            );
            false
        }
        Err(e) => {
            tracing::debug!("git {} CLI unavailable: {e}", label);
            false
        }
    }
}

/// Build a base `cosign` `Command` — the shared factory for signature / attestation
/// operations across `oci.rs`, `cli/module.rs`, and `upgrade.rs`.
///
/// Rationale: cosign is cfgd's controlled shell-out for Sigstore signature
/// verification, the same architectural category as [`git_cmd_safe`] for git.
/// Centralising the factory keeps invocation-site assumptions (stderr capture,
/// future env / timeout hardening) uniform and lets the module-boundary audit
/// point at one place instead of tracking every caller.
///
/// Callers add their own subcommand (`sign`, `verify-blob`, `verify-attestation`,
/// `attest`, etc.) and any additional flags.
pub fn cosign_cmd() -> std::process::Command {
    let mut cmd = std::process::Command::new("cosign");
    cmd.stderr(std::process::Stdio::piped());
    cmd
}

/// Best-effort detection of a local git repo's default branch.
///
/// Tries (in order) `origin/HEAD` symbolic-ref (the remote-tracking default),
/// then the local `HEAD` symbolic-ref. Returns `None` when the directory is not
/// a git repo, both refs are missing, or the `git` binary is unavailable.
///
/// Callers should supply their own fallback (cfgd convention: `"master"`).
pub fn detect_default_branch(repo_dir: &std::path::Path) -> Option<String> {
    let dir = repo_dir.display().to_string();

    let mut cmd = git_cmd_safe(None, None);
    cmd.args([
        "-C",
        &dir,
        "symbolic-ref",
        "--short",
        "refs/remotes/origin/HEAD",
    ])
    .stdout(std::process::Stdio::piped());
    if let Ok(output) = cmd.output()
        && output.status.success()
    {
        let raw = stdout_lossy_trimmed(&output);
        let stripped = raw.strip_prefix("origin/").unwrap_or(&raw);
        if !stripped.is_empty() {
            return Some(stripped.to_string());
        }
    }

    let mut cmd = git_cmd_safe(None, None);
    cmd.args(["-C", &dir, "symbolic-ref", "--short", "HEAD"])
        .stdout(std::process::Stdio::piped());
    if let Ok(output) = cmd.output()
        && output.status.success()
    {
        let branch = stdout_lossy_trimmed(&output);
        if !branch.is_empty() {
            return Some(branch);
        }
    }

    None
}

/// Git credential callback for git2 — handles SSH and HTTPS authentication.
/// Used by sources/, modules/, and daemon/ for all git operations.
///
/// Tries in order:
/// 1. SSH agent (for SSH URLs)
/// 2. SSH key files: `~/.ssh/id_ed25519`, `~/.ssh/id_rsa` (for SSH URLs)
/// 3. Git credential helper / GIT_ASKPASS (for HTTPS URLs)
/// 4. Default system credentials
pub fn git_ssh_credentials(
    _url: &str,
    username_from_url: Option<&str>,
    allowed_types: git2::CredentialType,
) -> std::result::Result<git2::Cred, git2::Error> {
    let username = username_from_url.unwrap_or("git");

    if allowed_types.contains(git2::CredentialType::SSH_KEY) {
        if let Ok(cred) = git2::Cred::ssh_key_from_agent(username) {
            return Ok(cred);
        }
        let home = home_dir_var().unwrap_or_default();
        for key_name in &["id_ed25519", "id_rsa", "id_ecdsa"] {
            let key_path = std::path::Path::new(&home).join(".ssh").join(key_name);
            if key_path.exists()
                && let Ok(cred) = git2::Cred::ssh_key(username, None, &key_path, None)
            {
                return Ok(cred);
            }
        }
    }

    if allowed_types.contains(git2::CredentialType::USER_PASS_PLAINTEXT) {
        return git2::Cred::credential_helper(
            &git2::Config::open_default()
                .map_err(|e| git2::Error::from_str(&format!("cannot open git config: {e}")))?,
            _url,
            username_from_url,
        );
    }

    if allowed_types.contains(git2::CredentialType::DEFAULT) {
        return git2::Cred::default();
    }

    Err(git2::Error::from_str("no suitable credentials found"))
}
