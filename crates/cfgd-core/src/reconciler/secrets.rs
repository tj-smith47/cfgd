use secrecy::ExposeSecret;

use crate::errors::Result;
use crate::expand_tilde;
use crate::providers::SecretAction;

impl<'a> super::Reconciler<'a> {
    pub(crate) fn apply_secret_action(
        &self,
        action: &SecretAction,
        config_dir: &std::path::Path,
        secret_env_collector: &mut Vec<(String, String)>,
    ) -> Result<String> {
        match action {
            SecretAction::Decrypt { source, target, .. } => {
                let backend = self
                    .registry
                    .secret_backend
                    .as_ref()
                    .ok_or(crate::errors::SecretError::SopsNotFound)?;

                let source_path =
                    crate::resolve_relative_path(source, config_dir).map_err(|_| {
                        crate::errors::SecretError::DecryptionFailed {
                            path: config_dir.join(source),
                            message: "source path contains traversal".to_string(),
                        }
                    })?;

                let decrypted = backend.decrypt_file(&source_path)?;

                let target_path = expand_tilde(target);
                crate::atomic_write(&target_path, decrypted.expose_secret().as_bytes())?;

                // Resource-id key, not display: `to_posix_string` folds on every
                // host (unlike `posix()`, a no-op on unix) so a Windows-written
                // key matches the POSIX one every other code path derives.
                Ok(format!(
                    "secret:decrypt:{}",
                    crate::to_posix_string(&target_path)
                ))
            }
            SecretAction::Resolve {
                provider,
                reference,
                target,
                ..
            } => {
                let secret_provider = self
                    .registry
                    .secret_providers
                    .iter()
                    .find(|p| p.name() == provider)
                    .ok_or_else(|| crate::errors::SecretError::ProviderNotAvailable {
                        provider: provider.clone(),
                        hint: format!("no provider '{}' registered", provider),
                    })?;

                let value = secret_provider.resolve(reference)?;

                let target_path = expand_tilde(target);
                crate::atomic_write(&target_path, value.expose_secret().as_bytes())?;

                Ok(format!(
                    "secret:resolve:{}:{}",
                    provider,
                    crate::to_posix_string(&target_path)
                ))
            }
            SecretAction::ResolveEnv {
                provider,
                reference,
                envs,
                ..
            } => {
                let secret_provider = self
                    .registry
                    .secret_providers
                    .iter()
                    .find(|p| p.name() == provider)
                    .ok_or_else(|| crate::errors::SecretError::ProviderNotAvailable {
                        provider: provider.clone(),
                        hint: format!("no provider '{}' registered", provider),
                    })?;

                let value = secret_provider.resolve(reference)?;

                // Each secret source resolves to exactly ONE value.
                // All env names in `envs` receive the same resolved value.
                // Expose the secret at the boundary where we need the plaintext for env injection.
                let plaintext = value.expose_secret().to_string();
                for env_name in envs {
                    secret_env_collector.push((env_name.clone(), plaintext.clone()));
                }

                Ok(format!(
                    "secret:resolve-env:{}:{}:[{}]",
                    provider,
                    reference,
                    envs.join(",")
                ))
            }
            SecretAction::Skip { source, .. } => Ok(format!("secret:skip:{}", source)),
        }
    }
}
