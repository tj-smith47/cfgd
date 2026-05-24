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

/// Build a `Command` for git suitable for LOCAL operations (config get/set,
/// tag verify, add, commit, log). Sets `GIT_TERMINAL_PROMPT=0` to prevent
/// any prompt-driven hang, but does NOT set `GIT_SSH_COMMAND` because no
/// network is involved. Use [`git_cmd_safe`] for any operation that talks to
/// a remote.
pub fn git_cmd_local() -> std::process::Command {
    let mut cmd = std::process::Command::new("git");
    cmd.env("GIT_TERMINAL_PROMPT", "0");
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

/// Env-var seam name for the cosign binary path. See [`tool_binary_name`].
pub const COSIGN_BIN_ENV: &str = "CFGD_COSIGN_BIN";

/// Build a base `cosign` `Command` — the shared factory for signature / attestation
/// operations across `oci.rs`, `cli/module.rs`, and `upgrade.rs`.
///
/// Rationale: cosign is cfgd's controlled shell-out for Sigstore signature
/// verification, the same architectural category as [`git_cmd_safe`] for git.
/// Centralising the factory keeps invocation-site assumptions (stderr capture,
/// future env / timeout hardening) uniform and lets the module-boundary audit
/// point at one place instead of tracking every caller.
///
/// The binary name honors `CFGD_COSIGN_BIN` for tests via [`tool_cmd`].
///
/// Callers add their own subcommand (`sign`, `verify-blob`, `verify-attestation`,
/// `attest`, etc.) and any additional flags.
pub fn cosign_cmd() -> std::process::Command {
    super::process::tool_cmd(COSIGN_BIN_ENV, "cosign")
}

/// Verify cosign is available, honoring the `CFGD_COSIGN_BIN` test seam.
/// Delegates to [`require_tool_with_seam`] to share the env-var-override logic
/// with every other shimmable tool in cfgd-core.
pub fn require_cosign() -> std::result::Result<(), String> {
    super::process::require_tool_with_seam(COSIGN_BIN_ENV, "cosign", None)
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

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::fs;

    /// Saves and restores the `CFGD_COSIGN_BIN` env var so tests stay isolated
    /// even when one panics. Pairs with `serial_test::serial` since env-var
    /// mutation is process-global.
    struct EnvVarGuard {
        key: &'static str,
        prior: Option<String>,
    }

    impl EnvVarGuard {
        fn capture(key: &'static str) -> Self {
            Self {
                key,
                prior: std::env::var(key).ok(),
            }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            // SAFETY: serial_test::serial gates execution; no concurrent reader.
            unsafe {
                match self.prior.take() {
                    Some(v) => std::env::set_var(self.key, v),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    #[test]
    fn git_cmd_local_sets_terminal_prompt_zero_and_no_ssh_env() {
        let cmd = git_cmd_local();
        let prog = std::path::Path::new(cmd.get_program())
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        assert_eq!(prog, "git", "program must resolve to `git`");

        let envs: std::collections::HashMap<&std::ffi::OsStr, Option<&std::ffi::OsStr>> =
            cmd.get_envs().collect();
        let term = envs
            .get(std::ffi::OsStr::new("GIT_TERMINAL_PROMPT"))
            .and_then(|v| v.as_deref())
            .and_then(|s| s.to_str());
        assert_eq!(
            term,
            Some("0"),
            "GIT_TERMINAL_PROMPT must be set to 0 to prevent prompt-driven hangs"
        );
        assert!(
            !envs.contains_key(std::ffi::OsStr::new("GIT_SSH_COMMAND")),
            "git_cmd_local is for local-only ops and must not configure GIT_SSH_COMMAND"
        );
    }

    #[test]
    #[serial]
    fn require_cosign_with_env_var_pointing_to_real_file_succeeds() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let bin = tmp.path().join("anything");
        fs::write(&bin, "").expect("write");

        let _guard = EnvVarGuard::capture("CFGD_COSIGN_BIN");
        // SAFETY: serial.
        unsafe {
            std::env::set_var("CFGD_COSIGN_BIN", &bin);
        }
        require_cosign().expect("env-var pointing to existing file → Ok");
    }

    #[test]
    #[serial]
    fn require_cosign_with_env_var_pointing_to_missing_file_errors_out() {
        let _guard = EnvVarGuard::capture("CFGD_COSIGN_BIN");
        // SAFETY: serial.
        unsafe {
            std::env::set_var("CFGD_COSIGN_BIN", "/no/such/file/at/all");
        }
        let err = require_cosign().expect_err("missing file → Err");
        assert!(
            err.contains("CFGD_COSIGN_BIN") && err.contains("not a file"),
            "error must call out env-var + missing-file: {err}"
        );
    }

    #[test]
    fn detect_default_branch_on_current_repo() {
        let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let result = detect_default_branch(repo_root);
        assert!(
            result.is_some(),
            "should detect default branch in the cfgd repo"
        );
        let branch = result.unwrap();
        assert!(!branch.is_empty(), "detected branch name must not be empty");
    }

    #[test]
    fn detect_default_branch_returns_none_for_non_repo() {
        let tmp = tempfile::TempDir::new().unwrap();
        let result = detect_default_branch(tmp.path());
        assert!(result.is_none(), "non-git directory must return None");
    }

    #[test]
    fn detect_default_branch_on_fresh_init_repo() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = git2::Repository::init(tmp.path()).unwrap();
        let sig = git2::Signature::now("test", "test@test.com").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        let result = detect_default_branch(tmp.path());
        assert!(result.is_some());
    }

    #[test]
    fn try_git_cmd_succeeds_on_version() {
        let ok = try_git_cmd(None, &["--version"], "version-check", None);
        assert!(ok, "git --version should succeed");
    }

    #[test]
    fn try_git_cmd_fails_on_invalid_subcommand() {
        let ok = try_git_cmd(None, &["not-a-real-subcommand-xyz"], "invalid-cmd", None);
        assert!(!ok, "invalid git subcommand should return false");
    }
}
