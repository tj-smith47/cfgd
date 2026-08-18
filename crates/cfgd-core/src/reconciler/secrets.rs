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

                // Keyed on the RESOLVED path: two declarations naming one file
                // by different relative spellings are one decryption.
                let decrypted = self.secrets.resolve_with(
                    backend.name(),
                    &crate::to_posix_string(&source_path),
                    || backend.decrypt_file(&source_path),
                )?;

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
                let value = self.resolve_provider_secret(provider, reference)?;

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
                let value = self.resolve_provider_secret(provider, reference)?;

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

    /// The value `provider` holds for `reference`, resolved at most once per run.
    ///
    /// Shared by the `Resolve` and `ResolveEnv` arms above, which are the SAME
    /// declared reference seen from its two occurrences — the file it lands in
    /// and the variables it exports. Resolving them independently spawned the
    /// provider CLI twice for one value.
    fn resolve_provider_secret(
        &self,
        provider: &str,
        reference: &str,
    ) -> Result<std::sync::Arc<secrecy::SecretString>> {
        let secret_provider = self
            .registry
            .secret_providers
            .iter()
            .find(|p| p.name() == provider)
            .ok_or_else(|| crate::errors::SecretError::ProviderNotAvailable {
                provider: provider.to_string(),
                hint: format!("no provider '{}' registered", provider),
            })?;

        self.secrets
            .resolve_with(provider, reference, || secret_provider.resolve(reference))
    }
}
