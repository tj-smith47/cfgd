// HashiCorp Vault Provider

use secrecy::SecretString;

use cfgd_core::errors::Result;
use cfgd_core::providers::SecretProvider;
use cfgd_core::{command_available_with_seam, tool_cmd};

use super::run_provider_cmd;

const VAULT_BIN_ENV: &str = "CFGD_VAULT_BIN";

pub struct VaultProvider;

impl SecretProvider for VaultProvider {
    fn name(&self) -> &str {
        "vault"
    }

    fn is_available(&self) -> bool {
        command_available_with_seam(VAULT_BIN_ENV, "vault")
    }

    fn resolve(&self, reference: &str) -> Result<SecretString> {
        // reference format: "secret/path#field"
        let (path, field) = if let Some(idx) = reference.rfind('#') {
            (&reference[..idx], &reference[idx + 1..])
        } else {
            (reference, "value")
        };

        run_provider_cmd(
            tool_cmd(VAULT_BIN_ENV, "vault")
                .arg("kv")
                .arg("get")
                // Equals-form so a user-supplied `field` can't be interpreted
                // as a separate flag by vault's arg parser.
                .arg(format!("-field={field}"))
                .arg("--")
                .arg(path),
            "vault",
            "install the Vault CLI: https://developer.hashicorp.com/vault/install",
            reference,
        )
    }
}
