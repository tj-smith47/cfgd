// LastPass Provider

use secrecy::SecretString;

use cfgd_core::command_available;
use cfgd_core::errors::Result;
use cfgd_core::providers::SecretProvider;

use super::run_provider_cmd;

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
            // Equals-form so a user-supplied `field` can't be interpreted as
            // a separate flag by lpass's arg parser.
            cmd.arg(format!("--field={field}"));
        } else {
            cmd.arg("--password");
        }
        cmd.arg("--").arg(item);

        run_provider_cmd(
            &mut cmd,
            "lastpass",
            "install the LastPass CLI: https://github.com/lastpass/lastpass-cli",
            reference,
        )
    }
}
