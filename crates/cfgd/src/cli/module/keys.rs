use super::*;
use cfgd_core::output::{Doc, Printer, Role};

pub fn cmd_module_keys_generate(
    v2_printer: &Printer,
    output_dir: Option<&str>,
) -> anyhow::Result<()> {
    if let Err(msg) = cfgd_core::require_tool_with_seam(
        "CFGD_COSIGN_BIN",
        "cosign",
        Some("install it from https://docs.sigstore.dev/cosign/installation/"),
    ) {
        v2_printer.emit(cfgd_core::output::error_doc(
            "cosign",
            "tool_missing",
            msg.clone(),
            serde_json::json!({}),
        ));
        anyhow::bail!(msg);
    }

    v2_printer.heading("Generate Cosign Key Pair");

    let dir = output_dir.unwrap_or(".");
    std::fs::create_dir_all(dir)?;

    // If COSIGN_PASSWORD is set, cosign reads it from the env and doesn't prompt.
    // Use null stdin in that case so it never blocks waiting for terminal input.
    let stdin_cfg = if std::env::var("COSIGN_PASSWORD").is_ok() {
        std::process::Stdio::null()
    } else {
        std::process::Stdio::inherit()
    };
    let status = cfgd_core::cosign_cmd()
        .args(["generate-key-pair"])
        .current_dir(dir)
        .stdin(stdin_cfg)
        .stdout(std::process::Stdio::inherit())
        // Override the default piped stderr: interactive key-pair generation
        // prompts the user and inherits the real terminal.
        .stderr(std::process::Stdio::inherit())
        .status()
        .map_err(|e| anyhow::anyhow!("failed to run cosign: {e}"))?;

    if !status.success() {
        v2_printer.emit(cfgd_core::output::error_doc(
            "cosign",
            "keygen_failed",
            "cosign generate-key-pair failed".to_string(),
            serde_json::json!({ "dir": dir }),
        ));
        anyhow::bail!("cosign generate-key-pair failed");
    }

    let key_dir = Path::new(dir);
    let mut private_key: Option<String> = None;
    let mut public_key: Option<String> = None;
    if key_dir.join("cosign.key").exists() {
        let priv_path = format!("{}/cosign.key", dir);
        let pub_path = format!("{}/cosign.pub", dir);
        v2_printer.kv_block([
            ("Private key", priv_path.clone()),
            ("Public key", pub_path.clone()),
        ]);
        v2_printer.hint("Sign with: cfgd module push --sign --key cosign.key ...");
        v2_printer.hint("Verify with: cosign verify --key cosign.pub <artifact>");
        private_key = Some(priv_path);
        public_key = Some(pub_path);
    }

    v2_printer.emit(
        Doc::new()
            .status(Role::Ok, "Generated cosign key pair")
            .with_data(serde_json::json!({
                "dir": dir,
                "privateKey": private_key,
                "publicKey": public_key,
            })),
    );

    Ok(())
}

pub fn cmd_module_keys_list(v2_printer: &Printer) -> anyhow::Result<()> {
    let locations = [
        ("./cosign.key", "./cosign.pub"),
        ("~/.cfgd/cosign.key", "~/.cfgd/cosign.pub"),
    ];

    let mut entries: Vec<super::KeyListEntry> = Vec::new();
    for (private, public) in &locations {
        let priv_path = cfgd_core::expand_tilde(Path::new(private));
        let pub_path = cfgd_core::expand_tilde(Path::new(public));

        if pub_path.exists() {
            let fingerprint = if priv_path.exists() {
                Some("private key: yes".to_string())
            } else {
                Some("private key: no".to_string())
            };
            let created = std::fs::metadata(&pub_path)
                .ok()
                .and_then(|m| m.modified().ok())
                .map(|t| {
                    let secs = t
                        .duration_since(std::time::SystemTime::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    cfgd_core::unix_secs_to_iso8601(secs)
                });
            entries.push(super::KeyListEntry {
                name: pub_path.display().to_string(),
                fingerprint,
                created,
            });
        }
    }

    let mut doc = Doc::new().heading("Signing Keys");

    if entries.is_empty() {
        doc = doc
            .status(Role::Info, "No signing keys found")
            .hint("Generate with: cfgd module keys generate");
    } else {
        let pairs: Vec<(String, String)> = entries
            .iter()
            .map(|e| (e.name.clone(), e.fingerprint.clone().unwrap_or_default()))
            .collect();
        doc = doc.kv_block(pairs);
    }

    v2_printer.emit(doc.with_data(&entries));
    Ok(())
}

pub fn cmd_module_keys_rotate(
    v2_printer: &Printer,
    dir: Option<&str>,
    artifacts: &[String],
) -> anyhow::Result<()> {
    if let Err(msg) = cfgd_core::require_tool_with_seam(
        "CFGD_COSIGN_BIN",
        "cosign",
        Some("install it from https://docs.sigstore.dev/cosign/installation/"),
    ) {
        v2_printer.emit(cfgd_core::output::error_doc(
            "cosign",
            "tool_missing",
            msg.clone(),
            serde_json::json!({}),
        ));
        anyhow::bail!(msg);
    }

    let key_dir = dir.unwrap_or(".");
    let old_key = Path::new(key_dir).join("cosign.key");
    let old_pub = Path::new(key_dir).join("cosign.pub");

    if !old_key.exists() {
        v2_printer.emit(cfgd_core::output::error_doc(
            key_dir,
            "key_not_found",
            format!(
                "No existing cosign.key found in {} — generate one first with: cfgd module keys generate",
                key_dir
            ),
            serde_json::json!({ "dir": key_dir }),
        ));
        anyhow::bail!(
            "No existing cosign.key found in {} — generate one first with: cfgd module keys generate",
            key_dir
        );
    }

    v2_printer.heading("Rotate Cosign Key Pair");

    // Back up old keys
    let backup_suffix = cfgd_core::utc_now_filename_safe();
    let backup_key = Path::new(key_dir).join(format!("cosign.key.{backup_suffix}"));
    let backup_pub = Path::new(key_dir).join(format!("cosign.pub.{backup_suffix}"));

    std::fs::rename(&old_key, &backup_key)?;
    v2_printer.status_simple(
        Role::Info,
        format!("Backed up old private key to {}", backup_key.display()),
    );
    if old_pub.exists() {
        std::fs::rename(&old_pub, &backup_pub)?;
        v2_printer.status_simple(
            Role::Info,
            format!("Backed up old public key to {}", backup_pub.display()),
        );
    }

    // Generate new key pair
    let status = cfgd_core::cosign_cmd()
        .args(["generate-key-pair"])
        .current_dir(key_dir)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        // Override the default piped stderr: interactive key-pair generation
        // prompts the user and inherits the real terminal.
        .stderr(std::process::Stdio::inherit())
        .status()
        .map_err(|e| anyhow::anyhow!("failed to run cosign: {e}"))?;

    if !status.success() {
        // Restore old keys on failure
        if backup_key.exists() {
            let _ = std::fs::rename(&backup_key, &old_key);
        }
        if backup_pub.exists() {
            let _ = std::fs::rename(&backup_pub, &old_pub);
        }
        v2_printer.emit(cfgd_core::output::error_doc(
            key_dir,
            "keygen_failed",
            "cosign generate-key-pair failed — old keys restored".to_string(),
            serde_json::json!({ "dir": key_dir }),
        ));
        anyhow::bail!("cosign generate-key-pair failed — old keys restored");
    }

    v2_printer.status_simple(Role::Ok, "Generated new key pair");

    // Re-sign artifacts with the new key
    let new_key_path = Path::new(key_dir).join("cosign.key");
    let mut resigned: Vec<String> = Vec::new();
    for artifact in artifacts {
        let sp = v2_printer.spinner(format!("Re-signing {artifact}..."));
        match cfgd_core::oci::sign_artifact(artifact, Some(&new_key_path.display().to_string())) {
            Ok(()) => {
                sp.finish_ok(format!("Re-signed {artifact}"));
                resigned.push(artifact.clone());
            }
            Err(e) => {
                sp.finish_fail(format!("Failed to re-sign {artifact}"))
                    .detail(e.to_string());
                v2_printer.emit(cfgd_core::output::error_doc(
                    "keys",
                    "resign_failed",
                    e.to_string(),
                    serde_json::json!({
                        "artifact": artifact,
                        "newKeyPath": new_key_path.display().to_string(),
                    }),
                ));
                return Err(anyhow::anyhow!("{e}"));
            }
        }
    }

    if artifacts.is_empty() {
        v2_printer.hint(
            "No artifacts specified — re-sign manually with: cfgd module push --sign --key cosign.key ...",
        );
    }

    let backup_pub_path = if old_pub.exists() || backup_pub.exists() {
        Some(backup_pub.display().to_string())
    } else {
        None
    };
    v2_printer.emit(
        Doc::new()
            .status(Role::Ok, "Key rotation complete")
            .with_data(serde_json::json!({
                "dir": key_dir,
                "backupPrivateKey": backup_key.display().to_string(),
                "backupPublicKey": backup_pub_path,
                "artifactsResigned": resigned,
            })),
    );
    Ok(())
}

/// Mask a value for display: show `***` with last 3 chars visible.
/// Short values (3 chars or fewer) are fully masked.
pub(crate) fn mask_value(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 3 {
        "***".to_string()
    } else {
        let suffix: String = chars[chars.len() - 3..].iter().collect();
        format!("***{}", suffix)
    }
}
