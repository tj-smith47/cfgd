// Age backend — fallback for opaque binary files where SOPS does not apply.

use std::path::{Path, PathBuf};

use secrecy::{ExposeSecret, SecretString};

use cfgd_core::errors::{Result, SecretError};
use cfgd_core::providers::SecretBackend;
use cfgd_core::{command_available_with_seam, tool_cmd};

use super::{extract_age_recipient, shell_split_editor};

/// Env-var seam for the `age` binary path. Production code reads no env var
/// and `Command::new` resolves `"age"` via PATH; tests set this var to a
/// `cfgd_core::test_helpers::ToolShim` script path. See `tool_binary_name`.
const AGE_BIN_ENV: &str = "CFGD_AGE_BIN";

/// Age-based secret backend for encrypting opaque binary files where SOPS
/// (which operates on structured data) doesn't apply.
pub struct AgeBackend {
    key_path: PathBuf,
}

impl AgeBackend {
    pub fn new(key_path: PathBuf) -> Self {
        Self { key_path }
    }

    pub(super) fn recipient_from_key(&self) -> Result<String> {
        let content =
            std::fs::read_to_string(&self.key_path).map_err(|_| SecretError::AgeKeyNotFound {
                path: self.key_path.clone(),
            })?;

        extract_age_recipient(&content).ok_or_else(|| {
            SecretError::AgeKeyNotFound {
                path: self.key_path.clone(),
            }
            .into()
        })
    }
}

impl SecretBackend for AgeBackend {
    fn name(&self) -> &str {
        "age"
    }

    fn is_available(&self) -> bool {
        self.key_path.exists() && command_available_with_seam(AGE_BIN_ENV, "age")
    }

    fn encrypt_file(&self, path: &Path) -> Result<()> {
        let recipient = self.recipient_from_key()?;
        let output_path = path.with_extension(
            path.extension()
                .map(|e| format!("{}.age", e.to_string_lossy()))
                .unwrap_or_else(|| "age".to_string()),
        );

        let output = tool_cmd(AGE_BIN_ENV, "age")
            .arg("--encrypt")
            .arg("--recipient")
            .arg(&recipient)
            .arg("--output")
            .arg(&output_path)
            .arg(path)
            .output()
            .map_err(|e| SecretError::EncryptionFailed {
                path: path.to_path_buf(),
                message: format!("failed to run age: {}", e),
            })?;

        if !output.status.success() {
            return Err(SecretError::EncryptionFailed {
                path: path.to_path_buf(),
                message: cfgd_core::stderr_lossy_trimmed(&output),
            }
            .into());
        }

        // Replace original with encrypted version
        std::fs::rename(&output_path, path).map_err(|e| SecretError::EncryptionFailed {
            path: path.to_path_buf(),
            message: format!("failed to replace original file: {}", e),
        })?;

        Ok(())
    }

    fn decrypt_file(&self, path: &Path) -> Result<SecretString> {
        let output = tool_cmd(AGE_BIN_ENV, "age")
            .arg("--decrypt")
            .arg("--identity")
            .arg(&self.key_path)
            .arg(path)
            .output()
            .map_err(|e| SecretError::DecryptionFailed {
                path: path.to_path_buf(),
                message: format!("failed to run age: {}", e),
            })?;

        if !output.status.success() {
            return Err(SecretError::DecryptionFailed {
                path: path.to_path_buf(),
                message: cfgd_core::stderr_lossy_trimmed(&output),
            }
            .into());
        }

        let s = String::from_utf8(output.stdout).map_err(|e| SecretError::DecryptionFailed {
            path: path.to_path_buf(),
            message: format!("invalid UTF-8 in decrypted output: {}", e),
        })?;
        Ok(SecretString::from(s))
    }

    fn edit_file(&self, path: &Path) -> Result<()> {
        // Decrypt to temp file, open editor, re-encrypt on save
        let decrypted = self.decrypt_file(path)?;

        let temp_dir = tempfile::TempDir::new().map_err(|e| SecretError::DecryptionFailed {
            path: path.to_path_buf(),
            message: format!("failed to create temp dir: {}", e),
        })?;

        let temp_file = temp_dir.path().join(
            path.file_name()
                .unwrap_or_else(|| std::ffi::OsStr::new("secret")),
        );

        std::fs::write(&temp_file, decrypted.expose_secret().as_bytes()).map_err(|e| {
            SecretError::DecryptionFailed {
                path: path.to_path_buf(),
                message: format!("failed to write temp file: {}", e),
            }
        })?;

        // Restrict permissions on decrypted secret (owner-only)
        cfgd_core::set_file_permissions(&temp_file, 0o600).map_err(|e| {
            SecretError::DecryptionFailed {
                path: path.to_path_buf(),
                message: format!("failed to set temp file permissions: {}", e),
            }
        })?;

        let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
        let parsed = shell_split_editor(&editor);
        let editor_cmd = parsed.first().map(|s| s.as_str()).unwrap_or("vi");
        let editor_args: Vec<&str> = parsed.iter().skip(1).map(|s| s.as_str()).collect();
        let status = std::process::Command::new(editor_cmd)
            .args(&editor_args)
            .arg(&temp_file)
            .stdin(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .status()
            .map_err(|e| SecretError::EncryptionFailed {
                path: path.to_path_buf(),
                message: format!("failed to open editor '{}': {}", editor, e),
            })?;

        if !status.success() {
            return Err(SecretError::EncryptionFailed {
                path: path.to_path_buf(),
                message: format!("editor '{}' exited with non-zero status", editor),
            }
            .into());
        }

        // Read edited content and write to original path
        let edited =
            std::fs::read_to_string(&temp_file).map_err(|e| SecretError::EncryptionFailed {
                path: path.to_path_buf(),
                message: format!("failed to read edited file: {}", e),
            })?;

        // If content hasn't changed, skip re-encryption
        if edited == *decrypted.expose_secret() {
            return Ok(());
        }

        cfgd_core::atomic_write_str(path, &edited).map_err(|e| SecretError::EncryptionFailed {
            path: path.to_path_buf(),
            message: format!("failed to write back: {}", e),
        })?;

        // Re-encrypt
        self.encrypt_file(path)?;

        // temp_dir auto-cleans on drop

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use cfgd_core::errors::{CfgdError, SecretError};
    use cfgd_core::providers::SecretBackend;

    use super::AgeBackend;

    // A minimal age key file with a `# public key:` comment line.
    const SAMPLE_KEY_FILE: &str = "\
# created: 2024-01-01T00:00:00Z\n\
# public key: age1ql3z7hjy54pw3hyww5ayyfg7zqgvc7w3j2elw8zmrj2kg5sfn9aqmcac8p\n\
AGE-SECRET-KEY-1QJQF0XE6P3X2P5VFQK8WMZDNW3F6KGPNXS4Y0EKJY3NQVJQQQ9SJ8LKZP\n";

    const SAMPLE_RECIPIENT: &str = "age1ql3z7hjy54pw3hyww5ayyfg7zqgvc7w3j2elw8zmrj2kg5sfn9aqmcac8p";

    fn write_key_file(dir: &tempfile::TempDir) -> PathBuf {
        let key_path = dir.path().join("age-key.txt");
        std::fs::write(&key_path, SAMPLE_KEY_FILE).expect("write key");
        key_path
    }

    // --- Pure / no-shim tests ------------------------------------------------

    #[test]
    fn name_returns_age() {
        let backend = AgeBackend::new(PathBuf::from("/nonexistent"));
        assert_eq!(backend.name(), "age");
    }

    #[test]
    fn recipient_from_key_parses_public_key_line() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let key_path = write_key_file(&dir);
        let backend = AgeBackend::new(key_path);
        let recipient = backend.recipient_from_key().expect("should parse key");
        assert_eq!(recipient, SAMPLE_RECIPIENT);
    }

    #[test]
    fn recipient_from_key_errors_when_file_missing() {
        let backend = AgeBackend::new(PathBuf::from("/does/not/exist/age-key.txt"));
        let err = backend.recipient_from_key().unwrap_err();
        assert!(
            matches!(err, CfgdError::Secret(SecretError::AgeKeyNotFound { .. })),
            "expected AgeKeyNotFound, got: {err}"
        );
    }

    #[test]
    fn recipient_from_key_errors_when_no_public_key_line() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let key_path = dir.path().join("age-key.txt");
        std::fs::write(
            &key_path,
            "# created: 2024-01-01T00:00:00Z\nno-public-key-here\n",
        )
        .expect("write");
        let backend = AgeBackend::new(key_path);
        let err = backend.recipient_from_key().unwrap_err();
        assert!(
            matches!(err, CfgdError::Secret(SecretError::AgeKeyNotFound { .. })),
            "expected AgeKeyNotFound, got: {err}"
        );
    }

    #[test]
    fn is_available_returns_false_when_key_missing() {
        let backend = AgeBackend::new(PathBuf::from("/does/not/exist/age-key.txt"));
        assert!(
            !backend.is_available(),
            "should be unavailable with missing key"
        );
    }

    // --- Shim-based tests (Unix only) ----------------------------------------

    #[cfg(unix)]
    mod shim_tests {
        use secrecy::ExposeSecret;
        use serial_test::serial;

        use cfgd_core::errors::{CfgdError, SecretError};
        use cfgd_core::providers::SecretBackend;
        use cfgd_core::test_helpers::{EnvVarGuard, ToolShim};

        use super::{AgeBackend, SAMPLE_RECIPIENT, write_key_file};

        const AGE_BIN_ENV: &str = "CFGD_AGE_BIN";

        #[test]
        #[serial]
        fn is_available_true_with_shim_and_existing_key() {
            let dir = tempfile::TempDir::new().expect("tempdir");
            let key_path = write_key_file(&dir);
            let _shim = ToolShim::install(AGE_BIN_ENV, 0, "", "");
            let backend = AgeBackend::new(key_path);
            assert!(
                backend.is_available(),
                "should be available with key + shim"
            );
        }

        #[test]
        #[serial]
        fn encrypt_file_happy_path_invokes_shim_with_correct_args() {
            let dir = tempfile::TempDir::new().expect("tempdir");
            let key_path = write_key_file(&dir);

            let secret_path = dir.path().join("secret.txt");
            std::fs::write(&secret_path, "plaintext").expect("write secret");

            // Pre-create the output path that encrypt_file will rename into place.
            // encrypt_file calls age with --output <path>.txt.age, then renames to <path>.txt.
            let output_path = secret_path.with_extension("txt.age");
            std::fs::write(&output_path, "fake-encrypted").expect("write fake output");

            let shim = ToolShim::install(AGE_BIN_ENV, 0, "", "");
            let backend = AgeBackend::new(key_path);
            backend
                .encrypt_file(&secret_path)
                .expect("encrypt should succeed");

            let log = shim.argv_log();
            assert_eq!(
                shim.invocation_count(),
                1,
                "age should be called exactly once"
            );
            assert!(
                log.contains("--encrypt"),
                "argv should contain --encrypt; got: {log}"
            );
            assert!(
                log.contains("--recipient"),
                "argv should contain --recipient; got: {log}"
            );
            assert!(
                log.contains(SAMPLE_RECIPIENT),
                "argv should contain recipient; got: {log}"
            );
            assert!(
                log.contains("--output"),
                "argv should contain --output; got: {log}"
            );
        }

        #[test]
        #[serial]
        fn encrypt_file_propagates_non_zero_exit() {
            let dir = tempfile::TempDir::new().expect("tempdir");
            let key_path = write_key_file(&dir);

            let secret_path = dir.path().join("secret.txt");
            std::fs::write(&secret_path, "plaintext").expect("write secret");

            let _shim = ToolShim::install(AGE_BIN_ENV, 1, "", "age encrypt failed");
            let backend = AgeBackend::new(key_path);
            let err = backend.encrypt_file(&secret_path).unwrap_err();
            let msg = err.to_string();
            assert!(
                msg.contains("age encrypt failed"),
                "error should include stderr; got: {msg}"
            );
            assert!(
                matches!(err, CfgdError::Secret(SecretError::EncryptionFailed { .. })),
                "expected EncryptionFailed, got: {msg}"
            );
        }

        #[test]
        #[serial]
        fn decrypt_file_happy_path_returns_decrypted_content() {
            let dir = tempfile::TempDir::new().expect("tempdir");
            let key_path = write_key_file(&dir);

            let enc_path = dir.path().join("secret.txt");
            std::fs::write(&enc_path, "irrelevant-ciphertext").expect("write file");

            let _shim = ToolShim::install(AGE_BIN_ENV, 0, "decrypted-content", "");
            let backend = AgeBackend::new(key_path);
            let secret = backend
                .decrypt_file(&enc_path)
                .expect("decrypt should succeed");
            assert_eq!(
                secret.expose_secret(),
                "decrypted-content",
                "decrypted content should match shim stdout"
            );
        }

        #[test]
        #[serial]
        fn decrypt_file_propagates_non_zero_exit() {
            let dir = tempfile::TempDir::new().expect("tempdir");
            let key_path = write_key_file(&dir);

            let enc_path = dir.path().join("secret.txt");
            std::fs::write(&enc_path, "ciphertext").expect("write file");

            let _shim = ToolShim::install(AGE_BIN_ENV, 1, "", "age decrypt failed");
            let backend = AgeBackend::new(key_path);
            let err = backend.decrypt_file(&enc_path).unwrap_err();
            let msg = err.to_string();
            assert!(
                msg.contains("age decrypt failed"),
                "error should include stderr; got: {msg}"
            );
            assert!(
                matches!(err, CfgdError::Secret(SecretError::DecryptionFailed { .. })),
                "expected DecryptionFailed, got: {msg}"
            );
        }

        #[test]
        #[serial]
        fn decrypt_file_invokes_shim_with_identity_and_path() {
            let dir = tempfile::TempDir::new().expect("tempdir");
            let key_path = write_key_file(&dir);

            let enc_path = dir.path().join("secret.age");
            std::fs::write(&enc_path, "ciphertext").expect("write file");

            let shim = ToolShim::install(AGE_BIN_ENV, 0, "content", "");
            let backend = AgeBackend::new(key_path.clone());
            backend
                .decrypt_file(&enc_path)
                .expect("decrypt should succeed");

            let log = shim.argv_log();
            assert!(
                log.contains("--decrypt"),
                "argv should contain --decrypt; got: {log}"
            );
            assert!(
                log.contains("--identity"),
                "argv should contain --identity; got: {log}"
            );
            assert!(
                log.contains(key_path.to_str().expect("valid path")),
                "argv should contain key path; got: {log}"
            );
        }

        #[test]
        #[serial]
        fn edit_file_skips_reencrypt_when_content_unchanged() {
            let dir = tempfile::TempDir::new().expect("tempdir");
            let key_path = write_key_file(&dir);

            // Write a key file used both for encrypting and for the age-key path
            let secret_path = dir.path().join("secret.txt");
            std::fs::write(&secret_path, "original-content").expect("write secret");

            // Shim: decrypt returns the same content, so editor sees it unchanged.
            // `true` as EDITOR exits 0 without modifying the temp file.
            // The shim stdout is the "decrypted" content that gets written to the temp file.
            let shim = ToolShim::install(AGE_BIN_ENV, 0, "original-content", "");
            let _editor_guard = EnvVarGuard::set("EDITOR", "true");
            let backend = AgeBackend::new(key_path);

            // edit_file decrypts (1 call), opens editor (no-op), sees content unchanged → no re-encrypt.
            backend
                .edit_file(&secret_path)
                .expect("edit should succeed");
            assert_eq!(
                shim.invocation_count(),
                1,
                "only decrypt call, no re-encrypt"
            );
        }

        #[test]
        #[serial]
        fn edit_file_reencrypts_when_content_changed() {
            let dir = tempfile::TempDir::new().expect("tempdir");
            let key_path = write_key_file(&dir);

            let secret_path = dir.path().join("secret.txt");
            std::fs::write(&secret_path, "original-content").expect("write secret");

            // Decrypt shim: returns "original-content".
            // The editor (a small shell script) writes "changed-content" to the temp file.
            // Then edit_file sees content changed → calls encrypt_file → shim called again.
            // We also need to pre-create the .txt.age output for the encrypt rename to succeed.
            let output_path = secret_path.with_extension("txt.age");

            // Use a shell command as EDITOR that overwrites the temp file.
            // We need an EDITOR that writes different content to $1 (the temp file path).
            let editor_script = dir.path().join("fake-editor.sh");
            std::fs::write(
                &editor_script,
                "#!/bin/sh\nprintf 'changed-content' > \"$1\"\n",
            )
            .expect("write editor");
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&editor_script)
                .expect("stat")
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&editor_script, perms).expect("chmod");

            // Shim exit 0 for both decrypt and the subsequent encrypt call.
            // For encrypt call we need the output file to exist (rename step).
            // We create it lazily by hooking decrypt stdout → but that can't create a side-effect file.
            // Instead: pre-create output_path before calling edit_file.
            std::fs::write(&output_path, "fake-encrypted").expect("pre-create output");

            let shim = ToolShim::install(AGE_BIN_ENV, 0, "original-content", "");
            let _editor_guard = EnvVarGuard::set("EDITOR", editor_script.to_str().expect("utf8"));
            let backend = AgeBackend::new(key_path);
            backend
                .edit_file(&secret_path)
                .expect("edit should succeed");

            // Decrypt (1) + encrypt (1) = 2 invocations.
            assert_eq!(
                shim.invocation_count(),
                2,
                "decrypt + re-encrypt = 2 shim calls"
            );
        }
    }
}
