use std::path::Path;

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

// ─────────────────────────────────────────────────────
// cfgd enroll — unified enrollment (token + key-based)
// ─────────────────────────────────────────────────────

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

    printer.key_value("Server", server_url);
    printer.key_value("Device ID", &device_id);

    let client = cfgd_core::server_client::ServerClient::new(server_url, None, &device_id);

    // Token-based enrollment (direct exchange)
    if let Some(token) = token {
        printer.header("Token Enrollment");
        printer.info("Exchanging bootstrap token for device credential...");

        let resp = client
            .enroll(token, printer)
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        return finish_enrollment(printer, server_url, &device_id, resp);
    }

    // Key-based enrollment (challenge-response)
    printer.header("Key-Based Enrollment");
    printer.key_value("Username", &username);

    // Check server enrollment method
    let info = client.enroll_info().map_err(|e| anyhow::anyhow!("{}", e))?;

    if info.method == "token" {
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
        match detect_ssh_key(printer) {
            Some(path) => (KeyType::Ssh, path),
            None => {
                anyhow::bail!(
                    "no SSH key found — provide --ssh-key <path> or --gpg-key <id>\n\
                     Checked: SSH agent, ~/.ssh/id_ed25519, ~/.ssh/id_rsa, ~/.ssh/id_ecdsa"
                );
            }
        }
    };

    printer.key_value(
        "Signing with",
        &format!("{} ({})", key_type.as_str().to_uppercase(), key_ref),
    );
    printer.newline();

    // Challenge-response
    let challenge = client
        .request_challenge(&username, printer)
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    printer.key_value("Challenge ID", &challenge.challenge_id);
    printer.key_value("Expires", &challenge.expires_at);

    let signature = match key_type {
        KeyType::Ssh => sign_with_ssh(&challenge.nonce, &key_ref)?,
        KeyType::Gpg => sign_with_gpg(&challenge.nonce, &key_ref)?,
    };

    printer.success("Challenge signed");

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

/// Shared enrollment completion: save credential, handle desired config, print next steps.
fn finish_enrollment(
    printer: &Printer,
    server_url: &str,
    device_id: &str,
    resp: cfgd_core::server_client::EnrollResponse,
) -> anyhow::Result<()> {
    printer.newline();
    printer.success(&format!("Enrolled as user '{}'", resp.username));
    if let Some(ref team) = resp.team {
        printer.key_value("Team", team);
    }
    printer.key_value("Device", &resp.device_id);

    let credential = cfgd_core::server_client::DeviceCredential {
        server_url: server_url.to_string(),
        device_id: device_id.to_string(),
        api_key: resp.api_key.clone(),
        username: resp.username.clone(),
        team: resp.team.clone(),
        enrolled_at: cfgd_core::utc_now_iso8601(),
    };

    match cfgd_core::server_client::save_credential(&credential) {
        Ok(path) => {
            printer.success(&format!("Credential saved to {}", path.display()));
        }
        Err(e) => {
            printer.error(&format!("Failed to save credential: {}", e));
            printer.warning("You will need to manually provide --api-key for future commands");
        }
    }

    if let Some(ref desired) = resp.desired_config {
        match cfgd_core::state::save_pending_server_config(desired) {
            Ok(path) => {
                printer.newline();
                printer.info(&format!(
                    "Server pushed desired config — saved to {}",
                    path.display()
                ));
                printer.info(MSG_RUN_APPLY);
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to save pending server config");
            }
        }
    }

    printer.newline();
    printer.header("Next Steps");
    printer.info("  cfgd checkin --server-url <url>  — report status to server");
    printer.info("  cfgd apply --dry-run             — preview configuration");
    printer.info("  cfgd apply                       — apply configuration");
    printer.info("  cfgd daemon install               — start background sync");

    Ok(())
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
            printer.info(&format!("Using SSH key from agent: {}", key.display()));
            return Some(key.to_string_lossy().to_string());
        }
    }

    // Fall back to on-disk keys
    if let Some(key) = first_existing_ssh_key(&ssh_dir) {
        printer.info(&format!("Using SSH key: {}", key.display()));
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
