// LastPass Provider

use secrecy::SecretString;

use cfgd_core::errors::Result;
use cfgd_core::providers::SecretProvider;
use cfgd_core::{command_available_with_seam, tool_cmd};

use super::run_provider_cmd;

const LPASS_BIN_ENV: &str = "CFGD_LPASS_BIN";

pub struct LastPassProvider;

impl SecretProvider for LastPassProvider {
    fn name(&self) -> &str {
        "lastpass"
    }

    fn is_available(&self) -> bool {
        command_available_with_seam(LPASS_BIN_ENV, "lpass")
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

        let mut cmd = tool_cmd(LPASS_BIN_ENV, "lpass");
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
