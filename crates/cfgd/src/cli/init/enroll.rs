use std::path::Path;

use cfgd_core::PathDisplayExt;
use cfgd_core::output::{Doc, Printer, Role};
use serde::Serialize;

use super::*;

/// Signing-key kind selected during enrollment. Replaces an earlier
/// `match key_type.as_str() { "ssh" | "gpg" | _ => unreachable!() }` so
/// the compiler enforces exhaustive handling.
#[derive(Copy, Clone)]
enum KeyType {
    Ssh,
    Gpg,
}

impl KeyType {
    fn as_str(self) -> &'static str {
        match self {
            KeyType::Ssh => "ssh",
            KeyType::Gpg => "gpg",
        }
    }
}

/// Structured-output payload for `cfgd enroll`. Drives `-o json|yaml|jsonpath|template`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrollOutput {
    pub server_url: String,
    pub device_id: String,
    pub username: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
}

/// Remediation hint for an enrollment error `kind`, rendered in human mode.
/// Shared by [`build_enroll_error`] and the `signing_failed` ctx carrier so the
/// hint text stays in one place.
fn enroll_error_hint(kind: &str) -> Option<&'static str> {
    match kind {
        "method_mismatch" => Some(
            "This server uses bootstrap token enrollment. Re-run with: cfgd enroll --server-url <url> --token <token>",
        ),
        "no_key" => Some(
            "Provide --ssh-key <path> or --gpg-key <id>. Checked: SSH agent, ~/.ssh/id_ed25519, ~/.ssh/id_rsa, ~/.ssh/id_ecdsa",
        ),
        "signing_failed" => {
            Some("Verify the signing key is accessible and the signing tool is installed.")
        }
        _ => None,
    }
}

/// Hints for an enrollment error `kind` as a `Vec`, suitable for the
/// `cli_error*_with_hints` carriers.
fn enroll_error_hints(kind: &str) -> Vec<String> {
    enroll_error_hint(kind)
        .map(|h| vec![h.to_string()])
        .into_iter()
        .flatten()
        .collect()
}

/// Build an enrollment error carrying the structured payload (`error: <kind>`,
/// `name`, plus `extras` fields) and the kind's remediation hint, routed through
/// the central sink. The single constructor for every `cmd_enroll` failure so the
/// payload shape and the per-kind hint stay in one place.
pub fn build_enroll_error(
    name: &str,
    kind: &'static str,
    message: impl Into<String>,
    extras: serde_json::Value,
) -> anyhow::Error {
    crate::cli::cli_error_with_hints(name, kind, message, extras, enroll_error_hints(kind))
}

// ─────────────────────────────────────────────────────
// cfgd enroll — unified enrollment (token + key-based)
// ─────────────────────────────────────────────────────

/// Run the unified enrollment flow. All emits route through `printer`,
/// including the internal `ServerClient::enroll` / `request_challenge` /
/// `submit_verification` calls.
pub(crate) fn cmd_enroll(
    printer: &Printer,
    server_url: &str,
    token: Option<&str>,
    ssh_key: Option<&str>,
    gpg_key: Option<&str>,
    username: Option<&str>,
) -> anyhow::Result<()> {
    let username = match username {
        Some(u) => u.to_string(),
        None => std::env::var("USER").unwrap_or_else(|_| "unknown".to_string()),
    };
    let device_id = default_device_id();

    printer.kv("Server", server_url);
    printer.kv("Device ID", &device_id);

    let client = cfgd_core::server_client::ServerClient::new(server_url, None, &device_id);

    // Token-based enrollment (direct exchange)
    if let Some(token) = token {
        printer.heading("Token Enrollment");
        printer.status_simple(
            Role::Info,
            "Exchanging bootstrap token for device credential...",
        );

        let resp = client
            .enroll(token, printer)
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        return finish_enrollment(printer, server_url, &device_id, resp);
    }

    // Key-based enrollment (challenge-response)
    printer.heading("Key-Based Enrollment");
    printer.kv("Username", &username);

    // Check server enrollment method
    let info = client.enroll_info().map_err(|e| anyhow::anyhow!("{}", e))?;

    if info.method == "token" {
        return Err(build_enroll_error(
            server_url,
            "method_mismatch",
            "This server uses bootstrap token enrollment. Run: cfgd enroll --server-url <url> --token <token>",
            serde_json::json!({
                "serverUrl": server_url,
                "serverMethod": info.method,
            }),
        ));
    }

    // Determine signing method
    let (key_type, key_ref) = if let Some(gpg_id) = gpg_key {
        (KeyType::Gpg, gpg_id.to_string())
    } else if let Some(ssh_path) = ssh_key {
        (KeyType::Ssh, ssh_path.to_string())
    } else {
        match detect_ssh_key(printer) {
            Some(path) => (KeyType::Ssh, path),
            None => {
                return Err(build_enroll_error(
                    &device_id,
                    "no_key",
                    "no SSH key found — provide --ssh-key <path> or --gpg-key <id>\n\
                     Checked: SSH agent, ~/.ssh/id_ed25519, ~/.ssh/id_rsa, ~/.ssh/id_ecdsa",
                    serde_json::json!({
                        "checked": [
                            "ssh-agent",
                            "~/.ssh/id_ed25519",
                            "~/.ssh/id_rsa",
                            "~/.ssh/id_ecdsa",
                        ],
                    }),
                ));
            }
        }
    };

    printer.kv(
        "Signing with",
        format!("{} ({})", key_type.as_str().to_uppercase(), key_ref),
    );

    // Challenge-response
    let challenge = client
        .request_challenge(&username, printer)
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    printer.kv("Challenge ID", &challenge.challenge_id);
    printer.kv("Expires", &challenge.expires_at);

    let signature = match key_type {
        KeyType::Ssh => sign_with_ssh(&challenge.nonce, &key_ref),
        KeyType::Gpg => sign_with_gpg(&challenge.nonce, &key_ref),
    }
    .map_err(|e| {
        let message = e.to_string();
        crate::cli::cli_error_ctx_with_hints(
            e,
            &key_ref,
            "signing_failed",
            cfgd_core::output::collapse_to_subject_line(&message),
            serde_json::json!({
                "keyType": key_type.as_str(),
                "keyRef": key_ref,
                "message": message,
            }),
            enroll_error_hints("signing_failed"),
        )
    })?;

    printer.status_simple(Role::Ok, "Challenge signed");

    let resp = client
        .submit_verification(
            &challenge.challenge_id,
            &signature,
            key_type.as_str(),
            printer,
        )
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    finish_enrollment(printer, server_url, &device_id, resp)
}

/// Shared enrollment completion: save credential, handle desired config, emit
/// the final buffered Doc carrying the structured payload.
fn finish_enrollment(
    printer: &Printer,
    server_url: &str,
    device_id: &str,
    resp: cfgd_core::server_client::EnrollResponse,
) -> anyhow::Result<()> {
    printer.status_simple(Role::Ok, format!("Enrolled as user '{}'", resp.username));
    if let Some(ref team) = resp.team {
        printer.kv("Team", team);
    }
    printer.kv("Device", &resp.device_id);

    let credential = build_device_credential(server_url, device_id, &resp);

    match cfgd_core::server_client::save_credential(&credential) {
        Ok(path) => {
            printer.status_simple(Role::Ok, format!("Credential saved to {}", path.posix()));
        }
        Err(e) => {
            printer.status_simple(
                Role::Fail,
                format!(
                    "Failed to save credential: {}",
                    cfgd_core::output::collapse_to_subject_line(&e),
                ),
            );
            printer.status_simple(
                Role::Warn,
                "You will need to manually provide --api-key for future commands",
            );
        }
    }

    if let Some(ref desired) = resp.desired_config {
        match cfgd_core::state::save_pending_server_config(desired) {
            Ok(path) => {
                printer.status_simple(
                    Role::Info,
                    format!("Server pushed desired config — saved to {}", path.posix()),
                );
                printer.status_simple(Role::Info, MSG_RUN_APPLY);
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to save pending server config");
            }
        }
    }

    let output = EnrollOutput {
        server_url: server_url.to_string(),
        device_id: device_id.to_string(),
        username: resp.username.clone(),
        team: resp.team.clone(),
    };
    printer.emit(build_enroll_final_doc(&output));

    Ok(())
}

/// Build the final enrollment `Doc` — a "Next Steps" section over the typed
/// `EnrollOutput` payload. Pure builder so snapshot tests can drive it
/// without standing up a mock server.
pub fn build_enroll_final_doc(output: &EnrollOutput) -> Doc {
    let lines = next_steps_lines();
    Doc::new()
        .section("Next Steps", |s| {
            lines.iter().fold(s, |s, line| s.bullet(*line))
        })
        .with_data(output)
}

/// Construct the persisted `DeviceCredential` from an `EnrollResponse`.
///
/// The credential structure is what cfgd writes to disk via
/// `save_credential` — its fields define the on-disk credential schema. The
/// `enrolled_at` timestamp is filled in here so every credential carries
/// a UTC ISO-8601 issue time the operator can correlate with server logs.
pub(super) fn build_device_credential(
    server_url: &str,
    device_id: &str,
    resp: &cfgd_core::server_client::EnrollResponse,
) -> cfgd_core::server_client::DeviceCredential {
    cfgd_core::server_client::DeviceCredential {
        server_url: server_url.to_string(),
        device_id: device_id.to_string(),
        api_key: resp.api_key.clone(),
        username: resp.username.clone(),
        team: resp.team.clone(),
        enrolled_at: cfgd_core::utc_now_iso8601(),
    }
}

/// The next-steps banner printed after a successful enrollment.
///
/// Pinned as a pure helper so the line set is testable and stable — the
/// CLI's "what do I do next?" affordance must not silently drop or reorder
/// these as the codebase evolves; doing so degrades the first-run UX.
/// Lines are bare command strings — the "Next Steps" section bullets supply
/// their own indent and `- ` prefix.
pub(super) fn next_steps_lines() -> &'static [&'static str] {
    &[
        "cfgd checkin --server-url <url>  — report status to server",
        "cfgd apply --dry-run             — preview configuration",
        "cfgd apply                       — apply configuration",
        "cfgd daemon install              — start background sync",
    ]
}

// ─────────────────────────────────────────────────────
// SSH/GPG signing helpers
// ─────────────────────────────────────────────────────

pub(super) fn detect_ssh_key(printer: &Printer) -> Option<String> {
    let ssh_dir = cfgd_core::expand_tilde(Path::new("~/.ssh"));

    // Try SSH agent first — when the agent has identities loaded, prefer
    // disk keys (matching agent semantics is impractical without the
    // agent socket, but the disk keys are what `ssh-keygen -Y sign` uses).
    if cfgd_core::command_available("ssh-add")
        && let Ok(output) = cfgd_core::command_output_with_timeout(
            std::process::Command::new("ssh-add").arg("-l"),
            std::time::Duration::from_secs(5),
        )
        && output.status.success()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Some(line) = stdout.lines().next()
            && !line.contains("no identities")
            && let Some(key) = first_existing_ssh_key(&ssh_dir)
        {
            printer.status_simple(
                Role::Info,
                format!("Using SSH key from agent: {}", key.posix()),
            );
            return Some(key.to_string_lossy().to_string());
        }
    }

    // Fall back to on-disk keys
    if let Some(key) = first_existing_ssh_key(&ssh_dir) {
        printer.status_simple(Role::Info, format!("Using SSH key: {}", key.posix()));
        return Some(key.to_string_lossy().to_string());
    }

    None
}

/// Scan `ssh_dir` for the first available key in priority order
/// (`id_ed25519` → `id_rsa` → `id_ecdsa`), preferring `<key>.pub` over the
/// private key in each round. Pure helper — split out so the priority
/// ordering and pub-vs-private fallback are testable against a tempdir.
pub(super) fn first_existing_ssh_key(ssh_dir: &Path) -> Option<std::path::PathBuf> {
    for key_name in &["id_ed25519", "id_rsa", "id_ecdsa"] {
        let pub_path = ssh_dir.join(format!("{key_name}.pub"));
        if pub_path.exists() {
            return Some(pub_path);
        }
        let key_path = ssh_dir.join(key_name);
        if key_path.exists() {
            return Some(key_path);
        }
    }
    None
}

pub(super) fn sign_with_ssh(nonce: &str, key_path: &str) -> anyhow::Result<String> {
    if !cfgd_core::command_available("ssh-keygen") {
        anyhow::bail!("ssh-keygen not found — is OpenSSH installed?");
    }

    let tmp_dir = tempfile::tempdir()?;
    let data_path = tmp_dir.path().join("challenge.txt");
    let sig_path = tmp_dir.path().join("challenge.txt.sig");

    std::fs::write(&data_path, nonce)?;

    // `ssh-keygen -Y sign` prompts on stdin for the key passphrase when the
    // private key is encrypted or missing, and can block on the agent socket.
    // In a headless enrollment (no tty, or a wedged agent) an inherited stdin
    // prompt never returns, hanging the enroll flow. Close stdin so any prompt
    // hits EOF and ssh-keygen exits non-zero, and bound the whole call with a
    // timeout as a hard backstop against any other block (e.g. the agent).
    let mut cmd = std::process::Command::new("ssh-keygen");
    cmd.args([
        "-Y",
        "sign",
        "-f",
        key_path,
        "-n",
        "cfgd-enroll",
        data_path.to_str().unwrap_or("challenge.txt"),
    ])
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::piped());

    let outcome =
        cfgd_core::command_output_with_timeout_outcome(&mut cmd, cfgd_core::COMMAND_TIMEOUT)
            .map_err(|e| anyhow::anyhow!("ssh-keygen not found: {e} — is OpenSSH installed?"))?;

    if outcome.timed_out {
        anyhow::bail!(
            "ssh-keygen -Y sign timed out after {}s — the SSH key may be prompting for a passphrase or the agent is unresponsive",
            cfgd_core::COMMAND_TIMEOUT.as_secs()
        );
    }

    if !outcome.output.status.success() {
        anyhow::bail!("ssh-keygen -Y sign failed — check that your SSH key is accessible");
    }

    let signature = std::fs::read_to_string(&sig_path)
        .map_err(|e| anyhow::anyhow!("failed to read SSH signature: {e}"))?;

    Ok(signature)
}

pub(super) fn sign_with_gpg(nonce: &str, gpg_key_id: &str) -> anyhow::Result<String> {
    if !cfgd_core::command_available("gpg") {
        anyhow::bail!("gpg not found — is GnuPG installed?");
    }

    let tmp_dir = tempfile::tempdir()?;
    let data_path = tmp_dir.path().join("challenge.txt");
    let sig_path = tmp_dir.path().join("challenge.txt.asc");

    std::fs::write(&data_path, nonce)?;

    // Sign with the user's own GnuPG keyring (default GNUPGHOME, or ~/.gnupg) — that
    // is where the requested secret key actually lives. We must NOT redirect --homedir
    // to a private dir: a fresh homedir holds no secret key, so signing always failed
    // with "no secret key". gpg reuses the user's running gpg-agent for any passphrase
    // pinentry, and we deliberately do not kill that agent afterward — it belongs to
    // the user's session, not to this process.
    // `--batch` makes gpg fail rather than prompt on its own stdin, but a
    // misconfigured gpg-agent/pinentry can still block. Close stdin and bound
    // the call with a timeout so enrollment can never hang on signing.
    let mut cmd = std::process::Command::new("gpg");
    cmd.args([
        "--batch",
        "--yes",
        "--detach-sign",
        "--armor",
        "-u",
        gpg_key_id,
        "-o",
        sig_path.to_str().unwrap_or("challenge.txt.asc"),
        data_path.to_str().unwrap_or("challenge.txt"),
    ])
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::piped());

    let outcome =
        cfgd_core::command_output_with_timeout_outcome(&mut cmd, cfgd_core::COMMAND_TIMEOUT)
            .map_err(|e| anyhow::anyhow!("gpg not found: {e} — is GPG installed?"))?;

    if outcome.timed_out {
        anyhow::bail!(
            "gpg --detach-sign timed out after {}s — the gpg-agent may be unresponsive",
            cfgd_core::COMMAND_TIMEOUT.as_secs()
        );
    }

    let sign_output = outcome.output;
    if !sign_output.status.success() {
        // Surface gpg's own stderr — it carries the actionable reason ("No secret key",
        // "no passphrase supplied in batch mode", …) that the user needs.
        anyhow::bail!(
            "gpg --detach-sign failed for secret key '{}': {}",
            gpg_key_id,
            cfgd_core::stderr_lossy_trimmed(&sign_output)
        );
    }

    let signature = std::fs::read_to_string(&sig_path)
        .map_err(|e| anyhow::anyhow!("failed to read GPG signature: {e}"))?;

    Ok(signature)
}
