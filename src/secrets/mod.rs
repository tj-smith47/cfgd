// Secrets management — SOPS integration, age fallback, external providers

use std::path::{Path, PathBuf};

use crate::errors::{Result, SecretError};
use crate::providers::{SecretBackend, SecretProvider};

/// Check if a command is available on the system.
fn command_available(cmd: &str) -> bool {
    std::process::Command::new(cmd)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// --- SOPS Backend (primary) ---

/// SOPS-based secret backend. Encrypts values within structured YAML/JSON files,
/// keeping keys visible for meaningful diffs. Wraps the `sops` CLI binary.
pub struct SopsBackend {
    age_key_path: Option<PathBuf>,
}

impl SopsBackend {
    pub fn new(age_key_path: Option<PathBuf>) -> Self {
        Self { age_key_path }
    }

    fn sops_command(&self) -> std::process::Command {
        let mut cmd = std::process::Command::new("sops");
        if let Some(ref key_path) = self.age_key_path {
            cmd.env("SOPS_AGE_KEY_FILE", key_path);
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
            .sops_command()
            .arg("--encrypt")
            .arg("--in-place")
            .arg(path)
            .output()
            .map_err(|_| SecretError::SopsNotFound)?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(SecretError::EncryptionFailed {
                path: path.to_path_buf(),
                message: stderr.trim().to_string(),
            }
            .into());
        }

        Ok(())
    }

    fn decrypt_file(&self, path: &Path) -> Result<String> {
        let output = self
            .sops_command()
            .arg("--decrypt")
            .arg(path)
            .output()
            .map_err(|_| SecretError::SopsNotFound)?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(SecretError::DecryptionFailed {
                path: path.to_path_buf(),
                message: stderr.trim().to_string(),
            }
            .into());
        }

        String::from_utf8(output.stdout).map_err(|e| {
            SecretError::DecryptionFailed {
                path: path.to_path_buf(),
                message: format!("invalid UTF-8 in decrypted output: {}", e),
            }
            .into()
        })
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

        if !status.success() {
            return Err(SecretError::EncryptionFailed {
                path: path.to_path_buf(),
                message: "sops edit exited with non-zero status".to_string(),
            }
            .into());
        }

        Ok(())
    }
}

// --- Age Backend (fallback for opaque binary files) ---

/// Age-based secret backend for encrypting opaque binary files where SOPS
/// (which operates on structured data) doesn't apply.
pub struct AgeBackend {
    key_path: PathBuf,
}

impl AgeBackend {
    pub fn new(key_path: PathBuf) -> Self {
        Self { key_path }
    }

    fn recipient_from_key(&self) -> Result<String> {
        let content =
            std::fs::read_to_string(&self.key_path).map_err(|_| SecretError::AgeKeyNotFound {
                path: self.key_path.clone(),
            })?;

        // age key files have a comment line with the public key: "# public key: age1..."
        for line in content.lines() {
            if let Some(pubkey) = line.strip_prefix("# public key: ") {
                return Ok(pubkey.trim().to_string());
            }
        }

        Err(SecretError::AgeKeyNotFound {
            path: self.key_path.clone(),
        }
        .into())
    }
}

impl SecretBackend for AgeBackend {
    fn name(&self) -> &str {
        "age"
    }

    fn is_available(&self) -> bool {
        self.key_path.exists() && command_available("age")
    }

    fn encrypt_file(&self, path: &Path) -> Result<()> {
        let recipient = self.recipient_from_key()?;
        let output_path = path.with_extension(
            path.extension()
                .map(|e| format!("{}.age", e.to_string_lossy()))
                .unwrap_or_else(|| "age".to_string()),
        );

        let output = std::process::Command::new("age")
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
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(SecretError::EncryptionFailed {
                path: path.to_path_buf(),
                message: stderr.trim().to_string(),
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

    fn decrypt_file(&self, path: &Path) -> Result<String> {
        let output = std::process::Command::new("age")
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
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(SecretError::DecryptionFailed {
                path: path.to_path_buf(),
                message: stderr.trim().to_string(),
            }
            .into());
        }

        String::from_utf8(output.stdout).map_err(|e| {
            SecretError::DecryptionFailed {
                path: path.to_path_buf(),
                message: format!("invalid UTF-8 in decrypted output: {}", e),
            }
            .into()
        })
    }

    fn edit_file(&self, path: &Path) -> Result<()> {
        // Decrypt to temp file, open editor, re-encrypt on save
        let decrypted = self.decrypt_file(path)?;

        let temp_dir = std::env::temp_dir().join(format!("cfgd-edit-{}", std::process::id()));
        std::fs::create_dir_all(&temp_dir).map_err(|e| SecretError::DecryptionFailed {
            path: path.to_path_buf(),
            message: format!("failed to create temp dir: {}", e),
        })?;

        let temp_file = temp_dir.join(
            path.file_name()
                .unwrap_or_else(|| std::ffi::OsStr::new("secret")),
        );

        std::fs::write(&temp_file, &decrypted).map_err(|e| SecretError::DecryptionFailed {
            path: path.to_path_buf(),
            message: format!("failed to write temp file: {}", e),
        })?;

        let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
        let status = std::process::Command::new(&editor)
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

        std::fs::write(path, &edited).map_err(|e| SecretError::EncryptionFailed {
            path: path.to_path_buf(),
            message: format!("failed to write back: {}", e),
        })?;

        // Re-encrypt
        self.encrypt_file(path)?;

        // Clean up temp dir
        let _ = std::fs::remove_dir_all(&temp_dir);

        Ok(())
    }
}

// --- 1Password Provider ---

pub struct OnePasswordProvider;

impl SecretProvider for OnePasswordProvider {
    fn name(&self) -> &str {
        "1password"
    }

    fn is_available(&self) -> bool {
        command_available("op")
    }

    fn resolve(&self, reference: &str) -> Result<String> {
        // reference format: "op://Vault/Item/Field" or legacy "Vault/Item/Field"
        let op_ref = if reference.starts_with("op://") {
            reference.to_string()
        } else {
            format!("op://{}", reference)
        };

        let output = std::process::Command::new("op")
            .arg("read")
            .arg(&op_ref)
            .output()
            .map_err(|_| SecretError::ProviderNotAvailable {
                provider: "1password".to_string(),
                hint: "install the 1Password CLI: https://developer.1password.com/docs/cli/get-started/".to_string(),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(SecretError::UnresolvableRef {
                reference: format!("{}: {}", reference, stderr.trim()),
            }
            .into());
        }

        let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(value)
    }
}

// --- Bitwarden Provider ---

pub struct BitwardenProvider;

impl SecretProvider for BitwardenProvider {
    fn name(&self) -> &str {
        "bitwarden"
    }

    fn is_available(&self) -> bool {
        command_available("bw")
    }

    fn resolve(&self, reference: &str) -> Result<String> {
        // reference format: "folder/item" or "folder/item/field"
        // Use `bw get` to retrieve the item
        let parts: Vec<&str> = reference.splitn(3, '/').collect();
        let item_name = if parts.len() >= 2 {
            parts[1]
        } else {
            reference
        };

        let output = std::process::Command::new("bw")
            .arg("get")
            .arg("password")
            .arg(item_name)
            .output()
            .map_err(|_| SecretError::ProviderNotAvailable {
                provider: "bitwarden".to_string(),
                hint: "install the Bitwarden CLI: https://bitwarden.com/help/cli/".to_string(),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(SecretError::UnresolvableRef {
                reference: format!("{}: {}", reference, stderr.trim()),
            }
            .into());
        }

        let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(value)
    }
}

// --- HashiCorp Vault Provider ---

pub struct VaultProvider;

impl SecretProvider for VaultProvider {
    fn name(&self) -> &str {
        "vault"
    }

    fn is_available(&self) -> bool {
        command_available("vault")
    }

    fn resolve(&self, reference: &str) -> Result<String> {
        // reference format: "secret/path#field"
        let (path, field) = if let Some(idx) = reference.rfind('#') {
            (&reference[..idx], &reference[idx + 1..])
        } else {
            (reference, "value")
        };

        let output = std::process::Command::new("vault")
            .arg("kv")
            .arg("get")
            .arg("-field")
            .arg(field)
            .arg(path)
            .output()
            .map_err(|_| SecretError::ProviderNotAvailable {
                provider: "vault".to_string(),
                hint: "install the Vault CLI: https://developer.hashicorp.com/vault/install"
                    .to_string(),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(SecretError::UnresolvableRef {
                reference: format!("{}: {}", reference, stderr.trim()),
            }
            .into());
        }

        let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(value)
    }
}

// --- Secret reference parsing ---

/// Parse a secret reference string to determine the provider.
/// Formats:
///   - `1password://Vault/Item/Field` → 1Password
///   - `bitwarden://folder/item` → Bitwarden
///   - `vault://secret/path#field` → HashiCorp Vault
///
/// Returns (provider_name, reference_path).
pub fn parse_secret_reference(source: &str) -> Option<(&str, &str)> {
    if let Some(rest) = source.strip_prefix("1password://") {
        Some(("1password", rest))
    } else if let Some(rest) = source.strip_prefix("bitwarden://") {
        Some(("bitwarden", rest))
    } else if let Some(rest) = source.strip_prefix("vault://") {
        Some(("vault", rest))
    } else {
        None
    }
}

/// Resolve `${secret:ref}` placeholders in a string using available secret providers.
/// Returns the string with all secret references resolved.
pub fn resolve_secret_refs(
    input: &str,
    providers: &[&dyn SecretProvider],
    backend: Option<&dyn SecretBackend>,
    config_dir: &Path,
) -> Result<String> {
    let mut result = input.to_string();
    let mut search_from = 0;

    while let Some(pos) = result[search_from..].find("${secret:") {
        let start = search_from + pos;

        let end = match result[start..].find('}') {
            Some(pos) => start + pos,
            None => break,
        };

        let ref_str = &result[start + 9..end]; // skip "${secret:"
        let resolved = resolve_single_ref(ref_str, providers, backend, config_dir)?;

        result.replace_range(start..=end, &resolved);
        search_from = start + resolved.len();
    }

    Ok(result)
}

fn resolve_single_ref(
    reference: &str,
    providers: &[&dyn SecretProvider],
    backend: Option<&dyn SecretBackend>,
    config_dir: &Path,
) -> Result<String> {
    // Check if it's a provider reference
    if let Some((provider_name, ref_path)) = parse_secret_reference(reference) {
        for provider in providers {
            if provider.name() == provider_name {
                if !provider.is_available() {
                    return Err(SecretError::ProviderNotAvailable {
                        provider: provider_name.to_string(),
                        hint: format!("install the {} CLI", provider_name),
                    }
                    .into());
                }
                return provider.resolve(ref_path);
            }
        }
        return Err(SecretError::ProviderNotAvailable {
            provider: provider_name.to_string(),
            hint: "no provider registered for this scheme".to_string(),
        }
        .into());
    }

    // Otherwise treat as a SOPS-encrypted file path
    if let Some(be) = backend {
        let path = if Path::new(reference).is_absolute() {
            PathBuf::from(reference)
        } else {
            config_dir.join(reference)
        };

        if path.exists() {
            return be.decrypt_file(&path);
        }
    }

    Err(SecretError::UnresolvableRef {
        reference: reference.to_string(),
    }
    .into())
}

/// Generate age identity and .sops.yaml for initial setup.
pub fn init_age_key(config_dir: &Path) -> Result<PathBuf> {
    let cfgd_config_dir = dirs_config_dir();
    std::fs::create_dir_all(&cfgd_config_dir).map_err(|e| SecretError::EncryptionFailed {
        path: cfgd_config_dir.clone(),
        message: format!("failed to create config directory: {}", e),
    })?;

    let key_path = cfgd_config_dir.join("age-key.txt");

    if !key_path.exists() {
        let output = std::process::Command::new("age-keygen")
            .arg("-o")
            .arg(&key_path)
            .output()
            .map_err(|e| SecretError::EncryptionFailed {
                path: key_path.clone(),
                message: format!(
                    "failed to run age-keygen (install age: https://github.com/FiloSottile/age): {}",
                    e
                ),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(SecretError::EncryptionFailed {
                path: key_path,
                message: format!("age-keygen failed: {}", stderr.trim()),
            }
            .into());
        }

        // Set restrictive permissions on the key file
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).map_err(
                |e| SecretError::EncryptionFailed {
                    path: key_path.clone(),
                    message: format!("failed to set key permissions: {}", e),
                },
            )?;
        }
    }

    // Generate .sops.yaml if it doesn't exist
    let sops_config_path = config_dir.join(".sops.yaml");
    if !sops_config_path.exists() {
        // Read the public key from the generated key file
        let key_content =
            std::fs::read_to_string(&key_path).map_err(|e| SecretError::EncryptionFailed {
                path: key_path.clone(),
                message: format!("failed to read age key: {}", e),
            })?;

        let mut recipient = String::new();
        for line in key_content.lines() {
            if let Some(pubkey) = line.strip_prefix("# public key: ") {
                recipient = pubkey.trim().to_string();
                break;
            }
        }

        if !recipient.is_empty() {
            let sops_config = format!("creation_rules:\n  - age: '{}'\n", recipient);
            std::fs::write(&sops_config_path, sops_config).map_err(|e| {
                SecretError::EncryptionFailed {
                    path: sops_config_path,
                    message: format!("failed to write .sops.yaml: {}", e),
                }
            })?;
        }
    }

    Ok(key_path)
}

/// Validate the secrets setup for `cfgd doctor`.
pub struct SecretsHealthCheck {
    pub sops_available: bool,
    pub sops_version: Option<String>,
    pub age_key_exists: bool,
    pub age_key_path: Option<PathBuf>,
    pub sops_config_exists: bool,
    pub sops_config_path: Option<PathBuf>,
    pub providers: Vec<(String, bool)>,
}

pub fn check_secrets_health(
    config_dir: &Path,
    age_key_override: Option<&Path>,
) -> SecretsHealthCheck {
    let sops_output = std::process::Command::new("sops").arg("--version").output();

    let (sops_available, sops_version) = match sops_output {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            (true, Some(version))
        }
        _ => (false, None),
    };

    let age_key_path = age_key_override
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs_config_dir().join("age-key.txt"));
    let age_key_exists = age_key_path.exists();

    let sops_config_path = config_dir.join(".sops.yaml");
    let sops_config_exists = sops_config_path.exists();

    let providers = vec![
        ("1password".to_string(), OnePasswordProvider.is_available()),
        ("bitwarden".to_string(), BitwardenProvider.is_available()),
        ("vault".to_string(), VaultProvider.is_available()),
    ];

    SecretsHealthCheck {
        sops_available,
        sops_version,
        age_key_exists,
        age_key_path: Some(age_key_path),
        sops_config_exists,
        sops_config_path: Some(sops_config_path),
        providers,
    }
}

/// Build the default secret backend based on config.
pub fn build_secret_backend(
    backend_name: &str,
    age_key_path: Option<PathBuf>,
) -> Box<dyn SecretBackend> {
    let key_path = age_key_path.unwrap_or_else(|| dirs_config_dir().join("age-key.txt"));

    match backend_name {
        "age" => Box::new(AgeBackend::new(key_path)),
        _ => Box::new(SopsBackend::new(Some(key_path))),
    }
}

/// Build all available secret providers.
pub fn build_secret_providers() -> Vec<Box<dyn SecretProvider>> {
    vec![
        Box::new(OnePasswordProvider),
        Box::new(BitwardenProvider),
        Box::new(VaultProvider),
    ]
}

fn dirs_config_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".config").join("cfgd")
    } else {
        PathBuf::from("/tmp/cfgd")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_1password_reference() {
        let result = parse_secret_reference("1password://Vault/Item/Field");
        assert_eq!(result, Some(("1password", "Vault/Item/Field")));
    }

    #[test]
    fn parse_bitwarden_reference() {
        let result = parse_secret_reference("bitwarden://folder/item");
        assert_eq!(result, Some(("bitwarden", "folder/item")));
    }

    #[test]
    fn parse_vault_reference() {
        let result = parse_secret_reference("vault://secret/path#field");
        assert_eq!(result, Some(("vault", "secret/path#field")));
    }

    #[test]
    fn parse_non_provider_reference() {
        let result = parse_secret_reference("secrets/env.yaml");
        assert_eq!(result, None);
    }

    #[test]
    fn sops_backend_reports_name() {
        let backend = SopsBackend::new(None);
        assert_eq!(backend.name(), "sops");
    }

    #[test]
    fn age_backend_reports_name() {
        let backend = AgeBackend::new(PathBuf::from("/tmp/test-key.txt"));
        assert_eq!(backend.name(), "age");
    }

    #[test]
    fn resolve_secret_refs_no_refs() {
        let result = resolve_secret_refs("no secrets here", &[], None, Path::new(".")).unwrap();
        assert_eq!(result, "no secrets here");
    }

    #[test]
    fn resolve_secret_refs_unresolvable() {
        let result =
            resolve_secret_refs("password=${secret:nonexistent}", &[], None, Path::new("."));
        assert!(result.is_err());
    }

    #[test]
    fn build_sops_backend_default() {
        let backend = build_secret_backend("sops", None);
        assert_eq!(backend.name(), "sops");
    }

    #[test]
    fn build_age_backend() {
        let backend = build_secret_backend("age", Some(PathBuf::from("/tmp/key.txt")));
        assert_eq!(backend.name(), "age");
    }

    #[test]
    fn build_providers_returns_three() {
        let providers = build_secret_providers();
        assert_eq!(providers.len(), 3);
        assert_eq!(providers[0].name(), "1password");
        assert_eq!(providers[1].name(), "bitwarden");
        assert_eq!(providers[2].name(), "vault");
    }

    #[test]
    fn health_check_runs_without_panic() {
        let dir = tempfile::tempdir().unwrap();
        let health = check_secrets_health(dir.path(), None);
        // sops may or may not be installed in test env, but should not panic
        assert!(health.age_key_path.is_some());
        assert_eq!(health.providers.len(), 3);
    }

    // Mock backend for testing resolve_secret_refs
    struct MockBackend;
    impl SecretBackend for MockBackend {
        fn name(&self) -> &str {
            "mock"
        }
        fn is_available(&self) -> bool {
            true
        }
        fn encrypt_file(&self, _path: &Path) -> Result<()> {
            Ok(())
        }
        fn decrypt_file(&self, _path: &Path) -> Result<String> {
            Ok("decrypted-value".to_string())
        }
        fn edit_file(&self, _path: &Path) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn resolve_file_secret_ref_with_backend() {
        let dir = tempfile::tempdir().unwrap();
        let secret_file = dir.path().join("secret.enc.yaml");
        std::fs::write(&secret_file, "encrypted data").unwrap();

        let backend = MockBackend;
        let result = resolve_secret_refs(
            "password=${secret:secret.enc.yaml}",
            &[],
            Some(&backend),
            dir.path(),
        )
        .unwrap();
        assert_eq!(result, "password=decrypted-value");
    }

    #[test]
    fn resolve_multiple_secret_refs() {
        let dir = tempfile::tempdir().unwrap();
        let f1 = dir.path().join("a.enc");
        let f2 = dir.path().join("b.enc");
        std::fs::write(&f1, "enc").unwrap();
        std::fs::write(&f2, "enc").unwrap();

        let backend = MockBackend;
        let result = resolve_secret_refs(
            "a=${secret:a.enc} b=${secret:b.enc}",
            &[],
            Some(&backend),
            dir.path(),
        )
        .unwrap();
        assert_eq!(result, "a=decrypted-value b=decrypted-value");
    }

    // Mock provider for testing
    struct MockProvider {
        value: String,
    }

    impl SecretProvider for MockProvider {
        fn name(&self) -> &str {
            "1password"
        }
        fn is_available(&self) -> bool {
            true
        }
        fn resolve(&self, _reference: &str) -> Result<String> {
            Ok(self.value.clone())
        }
    }

    #[test]
    fn resolve_provider_secret_ref() {
        let provider = MockProvider {
            value: "s3cr3t".to_string(),
        };
        let providers: Vec<&dyn SecretProvider> = vec![&provider];

        let result = resolve_secret_refs(
            "token=${secret:1password://Vault/Item/Field}",
            &providers,
            None,
            Path::new("."),
        )
        .unwrap();
        assert_eq!(result, "token=s3cr3t");
    }
}
