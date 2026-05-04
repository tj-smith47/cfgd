// Secrets management — SOPS integration, age fallback, external providers

use std::path::{Path, PathBuf};

use secrecy::{ExposeSecret, SecretString};

use cfgd_core::default_config_dir;
use cfgd_core::errors::{Result, SecretError};
use cfgd_core::providers::{SecretBackend, SecretProvider, parse_secret_reference};

mod age;
mod bitwarden;
mod lastpass;
mod onepassword;
mod sops;
mod vault;

pub use age::AgeBackend;
pub use bitwarden::BitwardenProvider;
pub use lastpass::LastPassProvider;
pub use onepassword::OnePasswordProvider;
pub use sops::SopsBackend;
pub use vault::VaultProvider;

#[cfg(test)]
mod tests;

/// Run a secret provider CLI command, mapping errors to SecretError variants.
/// Returns the output wrapped in `SecretString` for automatic zeroization on drop.
pub(super) fn run_provider_cmd(
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
pub(super) fn shell_split_editor(input: &str) -> Vec<String> {
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
pub(super) fn extract_age_recipient(content: &str) -> Option<String> {
    content.lines().find_map(|line| {
        line.strip_prefix("# public key: ")
            .map(|pk| pk.trim().to_string())
            .filter(|pk| !pk.is_empty())
    })
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
