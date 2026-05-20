use std::path::Path;

use cfgd_core::output::{Doc, Printer as PrinterV2, Role};
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

/// Doc emitted before an enrollment error bubbles to `main.rs::printer.error`.
/// `kind` tags the error class (e.g. `"method_mismatch"`, `"no_key"`,
/// `"signing_failed"`) and `payload` carries any additional fields. The
/// user-visible error string itself is rendered by `main.rs` so it appears
/// exactly once.
pub fn build_enroll_error_doc(kind: &'static str, payload: serde_json::Value) -> Doc {
    let mut doc = Doc::new();
    let hint = match kind {
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
    };
    if let Some(h) = hint {
        doc = doc.hint(h);
    }
    let mut obj = serde_json::Map::new();
    obj.insert("error".into(), serde_json::Value::String(kind.into()));
    if let serde_json::Value::Object(extra) = payload {
        for (k, v) in extra {
            obj.insert(k, v);
        }
    }
    doc.with_data(serde_json::Value::Object(obj))
}

// ─────────────────────────────────────────────────────
// cfgd enroll — unified enrollment (token + key-based)
// ─────────────────────────────────────────────────────

/// Run the unified enrollment flow. All emits route through `v2_printer`,
/// including the `ServerClient::enroll` / `request_challenge` /
/// `submit_verification` calls now that server_client is on v2.
pub(crate) fn cmd_enroll(
    v2_printer: &PrinterV2,
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

    v2_printer.kv("Server", server_url);
    v2_printer.kv("Device ID", &device_id);

    let client = cfgd_core::server_client::ServerClient::new(server_url, None, &device_id);

    // Token-based enrollment (direct exchange)
    if let Some(token) = token {
        v2_printer.heading("Token Enrollment");
        v2_printer.status_simple(
            Role::Info,
            "Exchanging bootstrap token for device credential...",
        );

        let resp = client
            .enroll(token, v2_printer)
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        return finish_enrollment(v2_printer, server_url, &device_id, resp);
    }

    // Key-based enrollment (challenge-response)
    v2_printer.heading("Key-Based Enrollment");
    v2_printer.kv("Username", &username);

    // Check server enrollment method
    let info = client.enroll_info().map_err(|e| anyhow::anyhow!("{}", e))?;

    if info.method == "token" {
        v2_printer.emit(build_enroll_error_doc(
            "method_mismatch",
            serde_json::json!({
                "serverUrl": server_url,
                "serverMethod": info.method,
            }),
        ));
        anyhow::bail!(
            "This server uses bootstrap token enrollment. Run: cfgd enroll --server-url <url> --token <token>"
        );
    }

    // Determine signing method
    let (key_type, key_ref) = if let Some(gpg_id) = gpg_key {
        (KeyType::Gpg, gpg_id.to_string())
    } else if let Some(ssh_path) = ssh_key {
        (KeyType::Ssh, ssh_path.to_string())
    } else {
        match detect_ssh_key(v2_printer) {
            Some(path) => (KeyType::Ssh, path),
            None => {
                v2_printer.emit(build_enroll_error_doc(
                    "no_key",
                    serde_json::json!({
                        "checked": [
                            "ssh-agent",
                            "~/.ssh/id_ed25519",
                            "~/.ssh/id_rsa",
                            "~/.ssh/id_ecdsa",
                        ],
                    }),
                ));
                anyhow::bail!(
                    "no SSH key found — provide --ssh-key <path> or --gpg-key <id>\n\
                     Checked: SSH agent, ~/.ssh/id_ed25519, ~/.ssh/id_rsa, ~/.ssh/id_ecdsa"
                );
            }
        }
    };

    v2_printer.kv(
        "Signing with",
        format!("{} ({})", key_type.as_str().to_uppercase(), key_ref),
    );

    // Challenge-response
    let challenge = client
        .request_challenge(&username, v2_printer)
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    v2_printer.kv("Challenge ID", &challenge.challenge_id);
    v2_printer.kv("Expires", &challenge.expires_at);

    let signature = match key_type {
        KeyType::Ssh => sign_with_ssh(&challenge.nonce, &key_ref),
        KeyType::Gpg => sign_with_gpg(&challenge.nonce, &key_ref),
    }
    .inspect_err(|e| {
        v2_printer.emit(build_enroll_error_doc(
            "signing_failed",
            serde_json::json!({
                "keyType": key_type.as_str(),
                "keyRef": key_ref,
                "message": e.to_string(),
            }),
        ));
    })?;

    v2_printer.status_simple(Role::Ok, "Challenge signed");

    let resp = client
        .submit_verification(
            &challenge.challenge_id,
            &signature,
            key_type.as_str(),
            v2_printer,
        )
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    finish_enrollment(v2_printer, server_url, &device_id, resp)
}

/// Shared enrollment completion: save credential, handle desired config, emit
/// the final buffered Doc carrying the structured payload.
fn finish_enrollment(
    v2_printer: &PrinterV2,
    server_url: &str,
    device_id: &str,
    resp: cfgd_core::server_client::EnrollResponse,
) -> anyhow::Result<()> {
    v2_printer.status_simple(Role::Ok, format!("Enrolled as user '{}'", resp.username));
    if let Some(ref team) = resp.team {
        v2_printer.kv("Team", team);
    }
    v2_printer.kv("Device", &resp.device_id);

    let credential = build_device_credential(server_url, device_id, &resp);

    match cfgd_core::server_client::save_credential(&credential) {
        Ok(path) => {
            v2_printer.status_simple(Role::Ok, format!("Credential saved to {}", path.display()));
        }
        Err(e) => {
            v2_printer.status_simple(Role::Fail, format!("Failed to save credential: {}", e));
            v2_printer.status_simple(
                Role::Warn,
                "You will need to manually provide --api-key for future commands",
            );
        }
    }

    if let Some(ref desired) = resp.desired_config {
        match cfgd_core::state::save_pending_server_config(desired) {
            Ok(path) => {
                v2_printer.status_simple(
                    Role::Info,
                    format!("Server pushed desired config — saved to {}", path.display()),
                );
                v2_printer.status_simple(Role::Info, MSG_RUN_APPLY);
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
    v2_printer.emit(build_enroll_final_doc(&output));

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

pub(super) fn detect_ssh_key(v2_printer: &PrinterV2) -> Option<String> {
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
            v2_printer.status_simple(
                Role::Info,
                format!("Using SSH key from agent: {}", key.display()),
            );
            return Some(key.to_string_lossy().to_string());
        }
    }

    // Fall back to on-disk keys
    if let Some(key) = first_existing_ssh_key(&ssh_dir) {
        v2_printer.status_simple(Role::Info, format!("Using SSH key: {}", key.display()));
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

    let status = std::process::Command::new("ssh-keygen")
        .args([
            "-Y",
            "sign",
            "-f",
            key_path,
            "-n",
            "cfgd-enroll",
            data_path.to_str().unwrap_or("challenge.txt"),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .status()
        .map_err(|e| anyhow::anyhow!("ssh-keygen not found: {e} — is OpenSSH installed?"))?;

    if !status.success() {
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

    let status = std::process::Command::new("gpg")
        .args([
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
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .status()
        .map_err(|e| anyhow::anyhow!("gpg not found: {e} — is GPG installed?"))?;

    if !status.success() {
        anyhow::bail!(
            "gpg --detach-sign failed — check that key '{}' is available",
            gpg_key_id
        );
    }

    let signature = std::fs::read_to_string(&sig_path)
        .map_err(|e| anyhow::anyhow!("failed to read GPG signature: {e}"))?;

    Ok(signature)
}
