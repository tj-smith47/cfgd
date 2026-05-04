// 1Password Provider

use secrecy::SecretString;

use cfgd_core::command_available;
use cfgd_core::errors::Result;
use cfgd_core::providers::SecretProvider;

use super::run_provider_cmd;

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
            std::process::Command::new("op")
                .arg("read")
                .arg("--")
                .arg(&op_ref),
            "1password",
            "install the 1Password CLI: https://developer.1password.com/docs/cli/get-started/",
            reference,
        )
    }
}
