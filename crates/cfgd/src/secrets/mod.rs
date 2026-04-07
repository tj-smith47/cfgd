// Secrets management — SOPS integration, age fallback, external providers

use std::path::{Path, PathBuf};

use secrecy::{ExposeSecret, SecretString};

use cfgd_core::errors::{Result, SecretError};
use cfgd_core::providers::{SecretBackend, SecretProvider, parse_secret_reference};
use cfgd_core::{command_available, default_config_dir};

/// Run a secret provider CLI command, mapping errors to SecretError variants.
/// Returns the output wrapped in `SecretString` for automatic zeroization on drop.
fn run_provider_cmd(
    cmd: &mut std::process::Command,
    provider: &str,
    hint: &str,
    reference: &str,
) -> Result<SecretString> {
    let output = cmd
        .output()
        .map_err(|_| SecretError::ProviderNotAvailable {
            provider: provider.to_string(),
            hint: hint.to_string(),
        })?;
    if !output.status.success() {
        return Err(SecretError::UnresolvableRef {
            reference: format!(
                "{}: {}",
                reference,
                cfgd_core::stderr_lossy_trimmed(&output)
            ),
        }
        .into());
    }
    Ok(SecretString::from(cfgd_core::stdout_lossy_trimmed(&output)))
}

/// Split an EDITOR value respecting single and double quotes.
/// e.g. `"/path/to my/editor" --wait` → ["/path/to my/editor", "--wait"]
fn shell_split_editor(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' | '"' => {
                let quote = c;
                for qc in chars.by_ref() {
                    if qc == quote {
                        break;
                    }
                    current.push(qc);
                }
            }
            c if c.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(c),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Extract the age public key from a key file's `# public key: age1...` comment.
fn extract_age_recipient(content: &str) -> Option<String> {
    content.lines().find_map(|line| {
        line.strip_prefix("# public key: ")
            .map(|pk| pk.trim().to_string())
            .filter(|pk| !pk.is_empty())
    })
}

// --- SOPS Backend (primary) ---

/// SOPS-based secret backend. Encrypts values within structured YAML/JSON files,
/// keeping keys visible for meaningful diffs. Wraps the `sops` CLI binary.
pub struct SopsBackend {
    age_key_path: Option<PathBuf>,
    sops_config: Option<PathBuf>,
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

    fn sops_command(&self) -> std::process::Command {
        let mut cmd = std::process::Command::new("sops");
        if let Some(ref key_path) = self.age_key_path {
            cmd.env("SOPS_AGE_KEY_FILE", key_path);
        }
        cmd
    }

    fn sops_encrypt_command(&self) -> std::process::Command {
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

// --- 1Password Provider ---

pub struct OnePasswordProvider;

impl SecretProvider for OnePasswordProvider {
    fn name(&self) -> &str {
        "1password"
    }

    fn is_available(&self) -> bool {
        command_available("op")
    }

    fn resolve(&self, reference: &str) -> Result<SecretString> {
        // reference format: "op://Vault/Item/Field" or legacy "Vault/Item/Field"
        let op_ref = if reference.starts_with("op://") {
            reference.to_string()
        } else {
            format!("op://{}", reference)
        };

        run_provider_cmd(
            std::process::Command::new("op").arg("read").arg(&op_ref),
            "1password",
            "install the 1Password CLI: https://developer.1password.com/docs/cli/get-started/",
            reference,
        )
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

    fn resolve(&self, reference: &str) -> Result<SecretString> {
        // reference format: "folder/item" or "folder/item/field"
        // Use `bw get` to retrieve the item
        let parts: Vec<&str> = reference.splitn(3, '/').collect();
        let item_name = if parts.len() >= 2 {
            parts[1]
        } else {
            reference
        };

        run_provider_cmd(
            std::process::Command::new("bw")
                .arg("get")
                .arg("password")
                .arg(item_name),
            "bitwarden",
            "install the Bitwarden CLI: https://bitwarden.com/help/cli/",
            reference,
        )
    }
}

// --- LastPass Provider ---

pub struct LastPassProvider;

impl SecretProvider for LastPassProvider {
    fn name(&self) -> &str {
        "lastpass"
    }

    fn is_available(&self) -> bool {
        command_available("lpass")
    }

    fn resolve(&self, reference: &str) -> Result<SecretString> {
        // reference format: "folder/item/field" or "item/field" or just "item"
        // Uses `lpass show --field <field> <item>` or `lpass show --password <item>`
        let parts: Vec<&str> = reference.rsplitn(2, '/').collect();
        let (item, field) = if parts.len() == 2 {
            (parts[1], Some(parts[0]))
        } else {
            (reference, None)
        };

        let mut cmd = std::process::Command::new("lpass");
        cmd.arg("show");
        if let Some(field) = field {
            cmd.arg("--field").arg(field);
        } else {
            cmd.arg("--password");
        }
        cmd.arg(item);

        run_provider_cmd(
            &mut cmd,
            "lastpass",
            "install the LastPass CLI: https://github.com/lastpass/lastpass-cli",
            reference,
        )
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

    fn resolve(&self, reference: &str) -> Result<SecretString> {
        // reference format: "secret/path#field"
        let (path, field) = if let Some(idx) = reference.rfind('#') {
            (&reference[..idx], &reference[idx + 1..])
        } else {
            (reference, "value")
        };

        run_provider_cmd(
            std::process::Command::new("vault")
                .arg("kv")
                .arg("get")
                .arg("-field")
                .arg(field)
                .arg(path),
            "vault",
            "install the Vault CLI: https://developer.hashicorp.com/vault/install",
            reference,
        )
    }
}

// --- Secret reference parsing ---

/// Resolve `${secret:ref}` placeholders in a string using available secret providers.
/// Returns the string with all secret references resolved.
///
/// Note: The returned `String` contains embedded secret material that will NOT be
/// zeroized on drop. This is intentional — the result is a mixed-content template
/// (e.g. `"password=s3cr3t"`) destined for file writes, not a pure secret.
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
        let plaintext = resolved.expose_secret();

        result.replace_range(start..=end, plaintext);
        search_from = start + plaintext.len();
    }

    Ok(result)
}

fn resolve_single_ref(
    reference: &str,
    providers: &[&dyn SecretProvider],
    backend: Option<&dyn SecretBackend>,
    config_dir: &Path,
) -> Result<SecretString> {
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
        let path =
            cfgd_core::resolve_relative_path(Path::new(reference), config_dir).map_err(|_| {
                SecretError::DecryptionFailed {
                    path: config_dir.join(reference),
                    message: "secret reference contains path traversal".to_string(),
                }
            })?;

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
    let cfgd_config_dir = default_config_dir();
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
            return Err(SecretError::EncryptionFailed {
                path: key_path,
                message: format!(
                    "age-keygen failed: {}",
                    cfgd_core::stderr_lossy_trimmed(&output)
                ),
            }
            .into());
        }

        // Set restrictive permissions on the key file
        cfgd_core::set_file_permissions(&key_path, 0o600).map_err(|e| {
            SecretError::EncryptionFailed {
                path: key_path.clone(),
                message: format!("failed to set key permissions: {}", e),
            }
        })?;
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

        let recipient = extract_age_recipient(&key_content).unwrap_or_default();

        if !recipient.is_empty() {
            let sops_config = format!("creation_rules:\n  - age: '{}'\n", recipient);
            cfgd_core::atomic_write_str(&sops_config_path, &sops_config).map_err(|e| {
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
            (true, Some(cfgd_core::stdout_lossy_trimmed(&output)))
        }
        _ => (false, None),
    };

    let age_key_path = age_key_override
        .map(PathBuf::from)
        .unwrap_or_else(|| default_config_dir().join("age-key.txt"));
    let age_key_exists = age_key_path.exists();

    let sops_config_path = config_dir.join(".sops.yaml");
    let sops_config_exists = sops_config_path.exists();

    let providers = vec![
        ("1password".to_string(), OnePasswordProvider.is_available()),
        ("bitwarden".to_string(), BitwardenProvider.is_available()),
        ("lastpass".to_string(), LastPassProvider.is_available()),
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
    config_dir: Option<&Path>,
) -> Box<dyn SecretBackend> {
    let key_path = age_key_path.unwrap_or_else(|| default_config_dir().join("age-key.txt"));

    match backend_name {
        "age" => Box::new(AgeBackend::new(key_path)),
        _ => {
            let backend = SopsBackend::new(Some(key_path));
            match config_dir {
                Some(dir) => Box::new(backend.with_config_dir(dir)),
                None => Box::new(backend),
            }
        }
    }
}

/// Build all available secret providers.
pub fn build_secret_providers() -> Vec<Box<dyn SecretProvider>> {
    vec![
        Box::new(OnePasswordProvider),
        Box::new(BitwardenProvider),
        Box::new(LastPassProvider),
        Box::new(VaultProvider),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_secret_reference_cases() {
        let cases: &[(&str, Option<(&str, &str)>)] = &[
            (
                "1password://Vault/Item/Field",
                Some(("1password", "Vault/Item/Field")),
            ),
            (
                "bitwarden://folder/item",
                Some(("bitwarden", "folder/item")),
            ),
            (
                "lastpass://folder/item/field",
                Some(("lastpass", "folder/item/field")),
            ),
            (
                "vault://secret/path#field",
                Some(("vault", "secret/path#field")),
            ),
            ("secrets/env.yaml", None),
        ];
        for (input, expected) in cases {
            assert_eq!(
                parse_secret_reference(input),
                *expected,
                "failed for {input:?}"
            );
        }
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
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("nonexistent"),
            "error should mention the unresolvable reference: {err}"
        );
        assert!(
            err.contains("unresolvable"),
            "error should indicate the ref is unresolvable: {err}"
        );
    }

    #[test]
    fn build_sops_backend_default() {
        let backend = build_secret_backend("sops", None, None);
        assert_eq!(backend.name(), "sops");
    }

    #[test]
    fn build_age_backend() {
        let backend = build_secret_backend("age", Some(PathBuf::from("/tmp/key.txt")), None);
        assert_eq!(backend.name(), "age");
    }

    #[test]
    fn build_providers_returns_four() {
        let providers = build_secret_providers();
        assert_eq!(providers.len(), 4);
        assert_eq!(providers[0].name(), "1password");
        assert_eq!(providers[1].name(), "bitwarden");
        assert_eq!(providers[2].name(), "lastpass");
        assert_eq!(providers[3].name(), "vault");
    }

    #[test]
    fn health_check_runs_without_panic() {
        let dir = tempfile::tempdir().unwrap();
        let health = check_secrets_health(dir.path(), None);
        // sops may or may not be installed in test env, but should not panic
        assert!(health.age_key_path.is_some());
        assert_eq!(health.providers.len(), 4);
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
        fn decrypt_file(&self, _path: &Path) -> Result<SecretString> {
            Ok(SecretString::from("decrypted-value".to_string()))
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
        fn resolve(&self, _reference: &str) -> Result<SecretString> {
            Ok(SecretString::from(self.value.clone()))
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

    #[test]
    fn resolve_secret_refs_text_without_markers() {
        let result = resolve_secret_refs("plain text no markers", &[], None, Path::new("."));
        assert_eq!(result.unwrap(), "plain text no markers");
    }

    #[test]
    fn resolve_secret_refs_preserves_surrounding_text() {
        let dir = tempfile::tempdir().unwrap();
        let secret_file = dir.path().join("key.enc");
        std::fs::write(&secret_file, "enc").unwrap();

        let backend = MockBackend;
        let result = resolve_secret_refs(
            "before ${secret:key.enc} after",
            &[],
            Some(&backend),
            dir.path(),
        )
        .unwrap();
        assert_eq!(result, "before decrypted-value after");
    }

    #[test]
    fn build_sops_backend_with_age_key() {
        let backend = build_secret_backend("sops", Some(PathBuf::from("/tmp/key.txt")), None);
        assert_eq!(backend.name(), "sops");
    }

    #[test]
    fn build_unknown_backend_falls_back_to_sops() {
        let backend = build_secret_backend("unknown", None, None);
        assert_eq!(backend.name(), "sops");
    }

    #[test]
    fn init_age_key_creates_key_file() {
        let dir = tempfile::tempdir().unwrap();
        if !cfgd_core::command_available("age-keygen") {
            // age not installed in CI — skip
            return;
        }
        let path = init_age_key(dir.path()).unwrap();
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("AGE-SECRET-KEY"));
    }

    #[test]
    fn provider_names_are_unique() {
        let providers = build_secret_providers();
        let mut names: Vec<&str> = providers.iter().map(|p| p.name()).collect();
        let before = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), before);
    }

    #[test]
    fn health_check_age_key_path_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let health = check_secrets_health(dir.path(), None);
        // Default age key path should be set even if file doesn't exist
        assert!(health.age_key_path.is_some());
    }

    // --- extract_age_recipient tests ---

    #[test]
    fn extract_age_recipient_valid_content() {
        let content =
            "# created: 2024-01-01\n# public key: age1abc123def456\nAGE-SECRET-KEY-1DEADBEEF\n";
        assert_eq!(
            extract_age_recipient(content),
            Some("age1abc123def456".to_string())
        );
    }

    #[test]
    fn extract_age_recipient_missing_public_key_line() {
        let content = "# created: 2024-01-01\nAGE-SECRET-KEY-1DEADBEEF\n";
        assert_eq!(extract_age_recipient(content), None);
    }

    #[test]
    fn extract_age_recipient_empty_content() {
        assert_eq!(extract_age_recipient(""), None);
    }

    #[test]
    fn extract_age_recipient_multiple_comment_lines() {
        let content = "\
# this is a comment
# another comment
# yet another
# public key: age1xyz789
AGE-SECRET-KEY-1STUFF\n";
        assert_eq!(
            extract_age_recipient(content),
            Some("age1xyz789".to_string())
        );
    }

    #[test]
    fn extract_age_recipient_trailing_whitespace_trimmed() {
        let content = "# public key: age1trailing   \n";
        assert_eq!(
            extract_age_recipient(content),
            Some("age1trailing".to_string())
        );
    }

    #[test]
    fn extract_age_recipient_only_prefix_no_key() {
        // The prefix is present but no key follows — empty key is rejected
        let content = "# public key: \n";
        assert_eq!(extract_age_recipient(content), None);
    }

    // --- resolve_secret_refs edge case tests ---

    #[test]
    fn resolve_secret_refs_unclosed_marker_passes_through() {
        // Unclosed ${secret: without closing } — the loop breaks and returns input as-is
        let result =
            resolve_secret_refs("value=${secret:oops_no_close", &[], None, Path::new(".")).unwrap();
        assert_eq!(result, "value=${secret:oops_no_close");
    }

    #[test]
    fn resolve_secret_refs_multiple_markers_with_provider() {
        use cfgd_core::test_helpers::MockSecretProvider;

        let provider = MockSecretProvider::new("vault").with_resolve_result("v1");
        let providers: Vec<&dyn SecretProvider> = vec![&provider];

        let result = resolve_secret_refs(
            "a=${secret:vault://p1} b=${secret:vault://p2} c=${secret:vault://p3}",
            &providers,
            None,
            Path::new("."),
        )
        .unwrap();
        assert_eq!(result, "a=v1 b=v1 c=v1");

        // All three references should have been resolved
        let calls = provider.resolve_calls.lock().unwrap();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0], "p1");
        assert_eq!(calls[1], "p2");
        assert_eq!(calls[2], "p3");
    }

    #[test]
    fn resolve_secret_refs_mixed_provider_and_file() {
        use cfgd_core::test_helpers::{MockSecretBackend, MockSecretProvider};

        let dir = tempfile::tempdir().unwrap();
        let secret_file = dir.path().join("creds.enc");
        std::fs::write(&secret_file, "encrypted").unwrap();

        let provider = MockSecretProvider::new("1password").with_resolve_result("op-secret");
        let backend = MockSecretBackend::new("sops").with_decrypt_result("file-secret");
        let providers: Vec<&dyn SecretProvider> = vec![&provider];

        let result = resolve_secret_refs(
            "pw=${secret:1password://v/i/f} db=${secret:creds.enc}",
            &providers,
            Some(&backend),
            dir.path(),
        )
        .unwrap();
        assert_eq!(result, "pw=op-secret db=file-secret");
    }

    #[test]
    fn resolve_secret_refs_unavailable_provider_errors() {
        use cfgd_core::test_helpers::MockSecretProvider;

        let provider = MockSecretProvider::new("vault").unavailable();
        let providers: Vec<&dyn SecretProvider> = vec![&provider];

        let result =
            resolve_secret_refs("x=${secret:vault://path}", &providers, None, Path::new("."));
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("vault"),
            "error should mention provider: {err}"
        );
    }

    #[test]
    fn resolve_secret_refs_unknown_provider_errors() {
        // No providers registered at all, but reference uses a provider scheme
        let result =
            resolve_secret_refs("x=${secret:unknown://something}", &[], None, Path::new("."));
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unknown://something"),
            "error should mention the unresolvable reference: {err}"
        );
        assert!(
            err.contains("unresolvable"),
            "error should indicate the reference is unresolvable: {err}"
        );
    }

    #[test]
    fn resolve_secret_refs_file_ref_path_traversal_errors() {
        let dir = tempfile::tempdir().unwrap();
        let backend = MockBackend;

        let result = resolve_secret_refs(
            "x=${secret:../../etc/shadow}",
            &[],
            Some(&backend),
            dir.path(),
        );
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("traversal"),
            "error should mention traversal: {err}"
        );
    }

    #[test]
    fn resolve_secret_refs_empty_input() {
        let result = resolve_secret_refs("", &[], None, Path::new(".")).unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn resolve_secret_refs_marker_at_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("s.enc");
        std::fs::write(&f, "data").unwrap();

        let backend = MockBackend;
        // Marker at the very start and very end of the string
        let result =
            resolve_secret_refs("${secret:s.enc}", &[], Some(&backend), dir.path()).unwrap();
        assert_eq!(result, "decrypted-value");
    }

    #[test]
    fn resolve_secret_refs_adjacent_markers() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.enc");
        std::fs::write(&f, "data").unwrap();

        let backend = MockBackend;
        let result = resolve_secret_refs(
            "${secret:a.enc}${secret:a.enc}",
            &[],
            Some(&backend),
            dir.path(),
        )
        .unwrap();
        assert_eq!(result, "decrypted-valuedecrypted-value");
    }

    // --- Mock verification tests using cfgd_core::test_helpers ---

    #[test]
    fn resolve_secret_ref_calls_backend_with_correct_path() {
        use cfgd_core::test_helpers::MockSecretBackend;

        let backend = MockSecretBackend::new("sops").with_decrypt_result("decrypted");
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("secret.enc.yaml"), "encrypted-data").unwrap();

        let result = resolve_secret_refs(
            "pw=${secret:secret.enc.yaml}",
            &[],
            Some(&backend),
            dir.path(),
        )
        .unwrap();
        assert_eq!(result, "pw=decrypted");

        let calls = backend.decrypt_calls.lock().unwrap();
        assert_eq!(
            calls.len(),
            1,
            "backend should have been called exactly once"
        );
        assert_eq!(
            calls[0],
            dir.path().join("secret.enc.yaml"),
            "backend should receive the resolved absolute path"
        );
    }

    #[test]
    fn resolve_secret_ref_calls_provider_with_correct_reference() {
        use cfgd_core::test_helpers::MockSecretProvider;

        let provider = MockSecretProvider::new("1password").with_resolve_result("provider-secret");
        let providers: Vec<&dyn SecretProvider> = vec![&provider];
        let dir = tempfile::tempdir().unwrap();

        let result = resolve_secret_refs(
            "token=${secret:1password://Vault/Item/Field}",
            &providers,
            None,
            dir.path(),
        )
        .unwrap();
        assert_eq!(result, "token=provider-secret");

        let calls = provider.resolve_calls.lock().unwrap();
        assert_eq!(
            calls.len(),
            1,
            "provider should have been called exactly once"
        );
        assert_eq!(
            calls[0], "Vault/Item/Field",
            "provider should receive the reference without the scheme prefix"
        );
    }

    #[test]
    fn resolve_secret_ref_backend_failure_propagates() {
        use cfgd_core::test_helpers::MockSecretBackend;

        let backend = MockSecretBackend::new("sops");
        backend.set_fail_decrypt(true);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("secret.enc"), "data").unwrap();

        let result = resolve_secret_refs("${secret:secret.enc}", &[], Some(&backend), dir.path());
        assert!(result.is_err(), "backend failure should propagate as error");
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("decrypt"),
            "error should mention decryption: {err}"
        );
    }

    #[test]
    fn resolve_secret_ref_provider_failure_propagates() {
        use cfgd_core::test_helpers::MockSecretProvider;

        let provider = MockSecretProvider::new("vault");
        provider.set_fail_resolve(true);
        let providers: Vec<&dyn SecretProvider> = vec![&provider];

        let result = resolve_secret_refs(
            "x=${secret:vault://secret/path}",
            &providers,
            None,
            Path::new("."),
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("secret/path"),
            "error should mention the secret reference: {err}"
        );
    }

    #[test]
    fn resolve_secret_ref_first_matching_provider_wins() {
        use cfgd_core::test_helpers::MockSecretProvider;

        let provider1 = MockSecretProvider::new("vault").with_resolve_result("from-first");
        let provider2 = MockSecretProvider::new("vault").with_resolve_result("from-second");
        let providers: Vec<&dyn SecretProvider> = vec![&provider1, &provider2];

        let result =
            resolve_secret_refs("x=${secret:vault://path}", &providers, None, Path::new("."))
                .unwrap();
        assert_eq!(result, "x=from-first");

        let calls1 = provider1.resolve_calls.lock().unwrap();
        let calls2 = provider2.resolve_calls.lock().unwrap();
        assert_eq!(calls1.len(), 1, "first matching provider should be called");
        assert_eq!(calls2.len(), 0, "second provider should not be called");
    }

    #[test]
    fn resolve_secret_ref_backend_receives_multiple_paths() {
        use cfgd_core::test_helpers::MockSecretBackend;

        let backend = MockSecretBackend::new("sops").with_decrypt_result("val");
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.enc"), "data").unwrap();
        std::fs::write(dir.path().join("b.enc"), "data").unwrap();

        let _ = resolve_secret_refs(
            "${secret:a.enc} ${secret:b.enc}",
            &[],
            Some(&backend),
            dir.path(),
        )
        .unwrap();

        let calls = backend.decrypt_calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], dir.path().join("a.enc"));
        assert_eq!(calls[1], dir.path().join("b.enc"));
    }

    #[test]
    fn extract_age_recipient_first_match_wins() {
        let content = "\
# public key: age1first
# public key: age1second
AGE-SECRET-KEY-1STUFF\n";
        assert_eq!(
            extract_age_recipient(content),
            Some("age1first".to_string()),
            "should return the first matching public key line"
        );
    }

    #[test]
    fn extract_age_recipient_whitespace_only_key() {
        let content = "# public key:    \n";
        assert_eq!(
            extract_age_recipient(content),
            None,
            "whitespace-only key should be rejected"
        );
    }

    // --- SopsBackend ---

    #[test]
    fn sops_backend_with_age_key_path() {
        let backend = SopsBackend::new(Some(PathBuf::from("/custom/age.txt")));
        assert_eq!(backend.name(), "sops");
        assert_eq!(backend.age_key_path, Some(PathBuf::from("/custom/age.txt")));
    }

    #[test]
    fn sops_backend_with_config_dir_existing_sops_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let sops_yaml = dir.path().join(".sops.yaml");
        std::fs::write(&sops_yaml, "creation_rules:\n  - age: 'age1test'\n").unwrap();

        let backend = SopsBackend::new(None).with_config_dir(dir.path());
        assert_eq!(backend.sops_config, Some(sops_yaml));
    }

    #[test]
    fn sops_backend_with_config_dir_no_sops_yaml() {
        let dir = tempfile::tempdir().unwrap();
        // No .sops.yaml created

        let backend = SopsBackend::new(None).with_config_dir(dir.path());
        assert_eq!(backend.sops_config, None);
    }

    // --- AgeBackend ---

    #[test]
    fn age_backend_not_available_without_key_file() {
        let backend = AgeBackend::new(PathBuf::from("/nonexistent/age-key.txt"));
        assert!(!backend.is_available());
    }

    #[test]
    fn age_backend_recipient_from_nonexistent_key_errors() {
        let backend = AgeBackend::new(PathBuf::from("/nonexistent/age-key.txt"));
        let result = backend.recipient_from_key();
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("age key not found") || err.contains("/nonexistent/age-key.txt"),
            "error should mention the missing key file: {err}"
        );
    }

    #[test]
    fn age_backend_recipient_from_valid_key() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("age-key.txt");
        std::fs::write(
            &key_path,
            "# created: 2024-01-01\n# public key: age1abc123\nAGE-SECRET-KEY-1TEST\n",
        )
        .unwrap();

        let backend = AgeBackend::new(key_path);
        let recipient = backend.recipient_from_key().unwrap();
        assert_eq!(recipient, "age1abc123");
    }

    #[test]
    fn age_backend_recipient_from_key_without_public_line() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("age-key.txt");
        std::fs::write(&key_path, "# created: 2024-01-01\nAGE-SECRET-KEY-1TEST\n").unwrap();

        let backend = AgeBackend::new(key_path.clone());
        let result = backend.recipient_from_key();
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("age key not found") || err.contains(key_path.to_str().unwrap()),
            "error should mention the key file path: {err}"
        );
    }

    // --- build_secret_backend with config_dir ---

    #[test]
    fn build_sops_backend_with_config_dir() {
        let dir = tempfile::tempdir().unwrap();
        let backend = build_secret_backend("sops", None, Some(dir.path()));
        assert_eq!(backend.name(), "sops");
    }

    #[test]
    fn build_age_backend_ignores_config_dir() {
        let dir = tempfile::tempdir().unwrap();
        let backend =
            build_secret_backend("age", Some(PathBuf::from("/tmp/key.txt")), Some(dir.path()));
        assert_eq!(backend.name(), "age");
    }

    // --- SecretProvider names ---

    #[test]
    fn one_password_provider_name() {
        assert_eq!(OnePasswordProvider.name(), "1password");
    }

    #[test]
    fn bitwarden_provider_name() {
        assert_eq!(BitwardenProvider.name(), "bitwarden");
    }

    #[test]
    fn lastpass_provider_name() {
        assert_eq!(LastPassProvider.name(), "lastpass");
    }

    #[test]
    fn vault_provider_name() {
        assert_eq!(VaultProvider.name(), "vault");
    }

    // --- check_secrets_health ---

    #[test]
    fn check_secrets_health_with_custom_age_key_path() {
        let dir = tempfile::tempdir().unwrap();
        let custom_key = dir.path().join("custom-age.txt");
        // Don't create the file — test that health reports existence correctly
        let health = check_secrets_health(dir.path(), Some(custom_key.as_path()));
        assert!(!health.age_key_exists);
        assert_eq!(health.age_key_path, Some(custom_key));
    }

    #[test]
    fn check_secrets_health_sops_config_detection() {
        let dir = tempfile::tempdir().unwrap();
        // No .sops.yaml
        let health1 = check_secrets_health(dir.path(), None);
        assert!(!health1.sops_config_exists);

        // Create .sops.yaml
        std::fs::write(dir.path().join(".sops.yaml"), "creation_rules: []\n").unwrap();
        let health2 = check_secrets_health(dir.path(), None);
        assert!(health2.sops_config_exists);
        assert_eq!(
            health2.sops_config_path,
            Some(dir.path().join(".sops.yaml"))
        );
    }

    #[test]
    fn check_secrets_health_provider_count() {
        let dir = tempfile::tempdir().unwrap();
        let health = check_secrets_health(dir.path(), None);
        // Should report all 4 providers
        assert_eq!(health.providers.len(), 4);
        let names: Vec<&str> = health.providers.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"1password"));
        assert!(names.contains(&"bitwarden"));
        assert!(names.contains(&"lastpass"));
        assert!(names.contains(&"vault"));
    }

    #[test]
    fn check_secrets_health_existing_age_key_reports_exists() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("present-key.txt");
        std::fs::write(&key_path, "# public key: age1test\nAGE-SECRET-KEY-1X\n").unwrap();
        let health = check_secrets_health(dir.path(), Some(&key_path));
        assert!(
            health.age_key_exists,
            "age key should exist at override path"
        );
        assert_eq!(health.age_key_path, Some(key_path));
    }

    #[test]
    fn resolve_secret_refs_provider_then_file_in_same_string() {
        // Mix a provider reference and a file reference in one string
        let dir = tempfile::tempdir().unwrap();
        let enc_file = dir.path().join("db.enc");
        std::fs::write(&enc_file, "encrypted").unwrap();

        let provider = MockProvider {
            value: "api-key-123".to_string(),
        };
        let providers: Vec<&dyn SecretProvider> = vec![&provider];
        let backend = MockBackend;

        let result = resolve_secret_refs(
            "key=${secret:1password://Vault/Item} db=${secret:db.enc}",
            &providers,
            Some(&backend),
            dir.path(),
        )
        .unwrap();
        assert_eq!(result, "key=api-key-123 db=decrypted-value");
    }

    #[test]
    fn sops_backend_with_config_dir_sets_sops_config() {
        let dir = tempfile::tempdir().unwrap();
        let sops_file = dir.path().join(".sops.yaml");
        std::fs::write(&sops_file, "creation_rules: []\n").unwrap();
        let backend = SopsBackend::new(None).with_config_dir(dir.path());
        // The backend should have internalized the config dir path
        assert_eq!(backend.name(), "sops");
    }

    // --- resolve_secret_refs with nested markers ---

    #[test]
    fn resolve_secret_refs_marker_without_scheme_not_a_file() {
        // A reference that isn't a provider scheme and doesn't exist as a file
        let result = resolve_secret_refs(
            "x=${secret:nonexistent_file.yaml}",
            &[],
            None,
            Path::new("."),
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("nonexistent_file.yaml"),
            "error should mention the unresolvable reference: {err}"
        );
        assert!(
            err.contains("unresolvable"),
            "error should indicate the ref is unresolvable: {err}"
        );
    }
}
