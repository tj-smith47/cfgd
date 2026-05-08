// Age Backend (fallback for opaque binary files)

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
