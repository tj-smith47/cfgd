use super::*;

pub(super) fn cmd_secret_encrypt(cli: &Cli, printer: &Printer, file: &Path) -> anyhow::Result<()> {
    printer.header("Secret Encrypt");

    let backend = get_secret_backend(cli, file)?;
    backend.encrypt_file(file)?;

    printer.newline();
    printer.success(&format!(
        "Encrypted {} via {}",
        file.display(),
        backend.name()
    ));

    Ok(())
}

pub(super) fn cmd_secret_decrypt(cli: &Cli, printer: &Printer, file: &Path) -> anyhow::Result<()> {
    let backend = get_secret_backend(cli, file)?;
    let decrypted = backend.decrypt_file(file)?;
    let plaintext = secrecy::ExposeSecret::expose_secret(&decrypted);

    if printer.is_structured() {
        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct SecretDecryptOutput<'a> {
            file: String,
            plaintext: &'a str,
        }
        printer.write_structured(&SecretDecryptOutput {
            file: file.display().to_string(),
            plaintext,
        });
        return Ok(());
    }

    // Plaintext must land on stdout so `cfgd secret decrypt foo.yaml > out.txt`
    // and `| pbcopy` work. `printer.info` routes to stderr (and is Quiet-suppressed
    // when `-o json` auto-Quiets the Printer), so we use `stdout_line` here the
    // same way `config get` does for its machine-readable output.
    printer.header("Secret Decrypt");
    printer.stdout_line(plaintext);

    Ok(())
}

pub(super) fn cmd_secret_edit(cli: &Cli, printer: &Printer, file: &Path) -> anyhow::Result<()> {
    printer.header("Secret Edit");

    let backend = get_secret_backend(cli, file)?;
    backend.edit_file(file)?;

    printer.newline();
    printer.success(&format!(
        "Edited and re-encrypted {} via {}",
        file.display(),
        backend.name()
    ));

    Ok(())
}

pub(super) fn cmd_secret_init(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    printer.header("Secret Init");

    let config_dir = config_dir(cli);
    let key_path = secrets::init_age_key(&config_dir)?;

    printer.newline();
    printer.success(&format!("Age key: {}", key_path.display()));

    let sops_config = config_dir.join(".sops.yaml");
    if sops_config.exists() {
        printer.success(&format!(".sops.yaml: {}", sops_config.display()));
    }

    printer.info("Secrets setup complete — files can now be encrypted with 'cfgd secret encrypt'");

    Ok(())
}
