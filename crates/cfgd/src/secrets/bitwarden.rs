// Bitwarden Provider

use secrecy::SecretString;

use cfgd_core::command_available;
use cfgd_core::errors::Result;
use cfgd_core::providers::SecretProvider;

use super::run_provider_cmd;

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
                .arg("--")
                .arg(item_name),
            "bitwarden",
            "install the Bitwarden CLI: https://bitwarden.com/help/cli/",
            reference,
        )
    }
}
