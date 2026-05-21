use super::*;
use cfgd_core::output::{Doc, Printer, Role};

pub fn cmd_module_keys_generate(printer: &Printer, output_dir: Option<&str>) -> anyhow::Result<()> {
    if let Err(msg) = cfgd_core::require_tool_with_seam(
        "CFGD_COSIGN_BIN",
        "cosign",
        Some("install it from https://docs.sigstore.dev/cosign/installation/"),
    ) {
        printer.emit(cfgd_core::output::error_doc(
            "cosign",
            "tool_missing",
            msg.clone(),
            serde_json::json!({}),
        ));
        anyhow::bail!(msg);
    }

    printer.heading("Generate Cosign Key Pair");

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
        printer.emit(cfgd_core::output::error_doc(
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
        printer.kv_block([
            ("Private key", priv_path.clone()),
            ("Public key", pub_path.clone()),
        ]);
        printer.hint("Sign with: cfgd module push --sign --key cosign.key ...");
        printer.hint("Verify with: cosign verify --key cosign.pub <artifact>");
        private_key = Some(priv_path);
        public_key = Some(pub_path);
    }

    printer.emit(
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

pub fn cmd_module_keys_list(printer: &Printer) -> anyhow::Result<()> {
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

    printer.emit(doc.with_data(&entries));
    Ok(())
}

pub fn cmd_module_keys_rotate(
    printer: &Printer,
    dir: Option<&str>,
    artifacts: &[String],
) -> anyhow::Result<()> {
    if let Err(msg) = cfgd_core::require_tool_with_seam(
        "CFGD_COSIGN_BIN",
        "cosign",
        Some("install it from https://docs.sigstore.dev/cosign/installation/"),
    ) {
        printer.emit(cfgd_core::output::error_doc(
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
        printer.emit(cfgd_core::output::error_doc(
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

    printer.heading("Rotate Cosign Key Pair");

    // Back up old keys
    let backup_suffix = cfgd_core::utc_now_filename_safe();
    let backup_key = Path::new(key_dir).join(format!("cosign.key.{backup_suffix}"));
    let backup_pub = Path::new(key_dir).join(format!("cosign.pub.{backup_suffix}"));

    std::fs::rename(&old_key, &backup_key)?;
    printer.status_simple(
        Role::Info,
        format!("Backed up old private key to {}", backup_key.display()),
    );
    if old_pub.exists() {
        std::fs::rename(&old_pub, &backup_pub)?;
        printer.status_simple(
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
        printer.emit(cfgd_core::output::error_doc(
            key_dir,
            "keygen_failed",
            "cosign generate-key-pair failed — old keys restored".to_string(),
            serde_json::json!({ "dir": key_dir }),
        ));
        anyhow::bail!("cosign generate-key-pair failed — old keys restored");
    }

    printer.status_simple(Role::Ok, "Generated new key pair");

    // Re-sign artifacts with the new key
    let new_key_path = Path::new(key_dir).join("cosign.key");
    let mut resigned: Vec<String> = Vec::new();
    for artifact in artifacts {
        let sp = printer.spinner(format!("Re-signing {artifact}..."));
        match cfgd_core::oci::sign_artifact(artifact, Some(&new_key_path.display().to_string())) {
            Ok(()) => {
                sp.finish_ok(format!("Re-signed {artifact}"));
                resigned.push(artifact.clone());
            }
            Err(e) => {
                sp.finish_fail(format!("Failed to re-sign {artifact}"))
                    .detail(e.to_string());
                printer.emit(cfgd_core::output::error_doc(
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
        printer.hint(
            "No artifacts specified — re-sign manually with: cfgd module push --sign --key cosign.key ...",
        );
    }

    let backup_pub_path = if old_pub.exists() || backup_pub.exists() {
        Some(backup_pub.display().to_string())
    } else {
        None
    };
    printer.emit(
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

#[cfg(test)]
mod tests {
    use cfgd_core::output::Printer;
    use cfgd_core::test_helpers::EnvVarGuard;
    use cfgd_core::with_test_home_guard;
    use serial_test::serial;

    use super::mask_value;
    use crate::cli::module::keys::{
        cmd_module_keys_generate, cmd_module_keys_list, cmd_module_keys_rotate,
    };

    // --- mask_value ---

    #[test]
    fn mask_value_empty_returns_sentinel() {
        assert_eq!(mask_value(""), "***");
    }

    #[test]
    fn mask_value_short_fully_masked() {
        assert_eq!(mask_value("ab"), "***");
        assert_eq!(mask_value("abc"), "***");
    }

    #[test]
    fn mask_value_long_shows_last_three() {
        let result = mask_value("supersecret");
        assert!(result.starts_with("***"), "must start with ***: {result}");
        assert!(
            result.ends_with("ret"),
            "must end with last 3 chars 'ret': {result}"
        );
        assert_eq!(result, "***ret");
    }

    #[test]
    fn mask_value_exactly_four_chars() {
        let result = mask_value("abcd");
        assert_eq!(result, "***bcd");
    }

    // --- cmd_module_keys_generate ---

    #[cfg(unix)]
    #[test]
    #[serial]
    fn generate_cosign_missing_emits_error_doc() {
        let _unset = EnvVarGuard::unset("CFGD_COSIGN_BIN");
        if cfgd_core::command_available("cosign") {
            return;
        }
        let (printer, cap) = Printer::for_test_doc();
        let err = cmd_module_keys_generate(&printer, None).unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("cosign"),
            "error should mention cosign: {err}"
        );
        let human = cap.human();
        assert!(
            human.to_lowercase().contains("tool_missing")
                || human.to_lowercase().contains("cosign"),
            "captured output should mention tool_missing or cosign: {human}"
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn generate_cosign_exits_nonzero_returns_err() {
        use cfgd_core::test_helpers::CosignTestShim;
        let _shim = CosignTestShim::builder()
            .with_exit(1)
            .with_stderr("cosign error")
            .install();
        let _pw = EnvVarGuard::set("COSIGN_PASSWORD", "");
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir_str = tmp.path().to_str().expect("utf8 path");
        let (printer, _cap) = Printer::for_test_doc();
        let err = cmd_module_keys_generate(&printer, Some(dir_str)).unwrap_err();
        assert!(
            err.to_string().contains("cosign generate-key-pair failed"),
            "unexpected error: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn generate_happy_path_writes_key_files() {
        use cfgd_core::test_helpers::CosignTestShim;
        let _shim = CosignTestShim::builder().with_keygen(true).install();
        let _pw = EnvVarGuard::set("COSIGN_PASSWORD", "");
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir_str = tmp.path().to_str().expect("utf8 path");
        let (printer, cap) = Printer::for_test_doc();
        cmd_module_keys_generate(&printer, Some(dir_str)).expect("generate should succeed");
        assert!(
            tmp.path().join("cosign.key").exists(),
            "cosign.key must exist"
        );
        assert!(
            tmp.path().join("cosign.pub").exists(),
            "cosign.pub must exist"
        );
        let key_content =
            std::fs::read_to_string(tmp.path().join("cosign.key")).expect("read cosign.key");
        assert_eq!(
            key_content, "fake-private-key-bytes",
            "key content should come from shim"
        );
        let json = cap.json().expect("doc should have json payload");
        assert_eq!(json["dir"], dir_str, "data payload dir mismatch");
    }

    // --- cmd_module_keys_list ---

    #[test]
    #[serial]
    fn list_empty_home_emits_no_keys_found() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = with_test_home_guard(tmp.path());
        let (printer, cap) = Printer::for_test_doc();
        cmd_module_keys_list(&printer).expect("list should not error");
        let human = cap.human();
        assert!(
            human.contains("No signing keys found"),
            "expected 'No signing keys found' in output: {human}"
        );
    }

    #[test]
    #[serial]
    fn list_with_home_pub_key_present_shows_key() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfgd_dir = tmp.path().join(".cfgd");
        std::fs::create_dir_all(&cfgd_dir).expect("create .cfgd dir");
        std::fs::write(cfgd_dir.join("cosign.pub"), "fake-pub-key").expect("write pub key");
        let _home = with_test_home_guard(tmp.path());
        let (printer, cap) = Printer::for_test_doc();
        cmd_module_keys_list(&printer).expect("list should not error");
        let json = cap.json().expect("doc should have json payload");
        let entries = json.as_array().expect("payload should be an array");
        assert!(!entries.is_empty(), "should find at least one key entry");
        let names: Vec<&str> = entries.iter().filter_map(|e| e["name"].as_str()).collect();
        assert!(
            names.iter().any(|n| n.contains("cosign.pub")),
            "key name should contain 'cosign.pub': {names:?}"
        );
    }

    // --- cmd_module_keys_rotate ---

    #[cfg(unix)]
    #[test]
    #[serial]
    fn rotate_errors_when_no_existing_key() {
        use cfgd_core::test_helpers::CosignTestShim;
        let _shim = CosignTestShim::builder().with_keygen(true).install();
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir_str = tmp.path().to_str().expect("utf8 path");
        let (printer, _cap) = Printer::for_test_doc();
        let err = cmd_module_keys_rotate(&printer, Some(dir_str), &[]).unwrap_err();
        assert!(
            err.to_string().contains("No existing cosign.key"),
            "unexpected error: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn rotate_happy_path_backs_up_and_generates_new() {
        use cfgd_core::test_helpers::CosignTestShim;
        let _shim = CosignTestShim::builder().with_keygen(true).install();
        let _pw = EnvVarGuard::set("COSIGN_PASSWORD", "");
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir_str = tmp.path().to_str().expect("utf8 path");
        std::fs::write(tmp.path().join("cosign.key"), "old-private-key").expect("write old key");
        std::fs::write(tmp.path().join("cosign.pub"), "old-public-key").expect("write old pub");
        let (printer, cap) = Printer::for_test_doc();
        cmd_module_keys_rotate(&printer, Some(dir_str), &[]).expect("rotate should succeed");
        let new_key_content =
            std::fs::read_to_string(tmp.path().join("cosign.key")).expect("read new cosign.key");
        assert_eq!(
            new_key_content, "fake-private-key-bytes",
            "new key should come from shim"
        );
        let backup_key_exists = std::fs::read_dir(tmp.path())
            .expect("read dir")
            .filter_map(|e| e.ok())
            .any(|e| {
                let name = e.file_name();
                let n = name.to_string_lossy();
                n.starts_with("cosign.key.") && n != "cosign.key"
            });
        assert!(backup_key_exists, "a backup cosign.key.* file must exist");
        let json = cap.json().expect("doc should have json payload");
        assert_eq!(
            json["dir"], dir_str,
            "rotation doc should record the key dir"
        );
        assert!(
            json["backupPrivateKey"].as_str().is_some(),
            "backupPrivateKey must be in the payload"
        );
    }
}
