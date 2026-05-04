// SOPS Backend (primary)

use std::path::{Path, PathBuf};

use secrecy::SecretString;

use cfgd_core::command_available;
use cfgd_core::errors::{Result, SecretError};
use cfgd_core::providers::SecretBackend;

/// SOPS-based secret backend. Encrypts values within structured YAML/JSON files,
/// keeping keys visible for meaningful diffs. Wraps the `sops` CLI binary.
pub struct SopsBackend {
    pub(super) age_key_path: Option<PathBuf>,
    pub(super) sops_config: Option<PathBuf>,
}

impl SopsBackend {
    pub fn new(age_key_path: Option<PathBuf>) -> Self {
        Self {
            age_key_path,
            sops_config: None,
        }
    }

    pub fn with_config_dir(mut self, config_dir: &Path) -> Self {
        let sops_yaml = config_dir.join(".sops.yaml");
        if sops_yaml.exists() {
            self.sops_config = Some(sops_yaml);
        }
        self
    }

    pub(super) fn sops_command(&self) -> std::process::Command {
        let mut cmd = std::process::Command::new("sops");
        if let Some(ref key_path) = self.age_key_path {
            cmd.env("SOPS_AGE_KEY_FILE", key_path);
        }
        cmd
    }

    pub(super) fn sops_encrypt_command(&self) -> std::process::Command {
        let mut cmd = self.sops_command();
        if let Some(ref config_path) = self.sops_config {
            cmd.arg("--config").arg(config_path);
        }
        cmd
    }
}

impl SecretBackend for SopsBackend {
    fn name(&self) -> &str {
        "sops"
    }

    fn is_available(&self) -> bool {
        command_available("sops")
    }

    fn encrypt_file(&self, path: &Path) -> Result<()> {
        let output = self
            .sops_encrypt_command()
            .arg("--encrypt")
            .arg("--in-place")
            .arg(path)
            .output()
            .map_err(|_| SecretError::SopsNotFound)?;

        if !output.status.success() {
            return Err(SecretError::EncryptionFailed {
                path: path.to_path_buf(),
                message: cfgd_core::stderr_lossy_trimmed(&output),
            }
            .into());
        }

        Ok(())
    }

    fn decrypt_file(&self, path: &Path) -> Result<SecretString> {
        let output = self
            .sops_command()
            .arg("--decrypt")
            .arg(path)
            .output()
            .map_err(|_| SecretError::SopsNotFound)?;

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
        let status = self
            .sops_command()
            .arg(path)
            .stdin(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .status()
            .map_err(|_| SecretError::SopsNotFound)?;

        // sops exits with code 200 when the editor didn't change the file
        // (e.g., EDITOR=true). Treat this as a no-op success.
        if !status.success() && status.code() != Some(200) {
            return Err(SecretError::EncryptionFailed {
                path: path.to_path_buf(),
                message: "sops edit exited with non-zero status".to_string(),
            }
            .into());
        }

        Ok(())
    }
}
