use super::*;
use cfgd_core::output_v2::{Doc, Printer as PrinterV2, Role};

pub fn cmd_secret_encrypt(cli: &Cli, v2_printer: &PrinterV2, file: &Path) -> anyhow::Result<()> {
    let backend = match get_secret_backend(cli, file) {
        Ok(b) => b,
        Err(e) => {
            v2_printer.emit(cfgd_core::output_v2::error_doc(
                &file.display().to_string(),
                "backend_unavailable",
                format!("{}", e),
                serde_json::json!({ "path": file.display().to_string() }),
            ));
            return Err(e);
        }
    };
    let backend_name = backend.name().to_string();

    if let Err(e) = backend.encrypt_file(file) {
        v2_printer.emit(cfgd_core::output_v2::error_doc(
            &file.display().to_string(),
            "encryption_failed",
            format!("{}", e),
            serde_json::json!({
                "path": file.display().to_string(),
                "backend": backend_name,
            }),
        ));
        return Err(e.into());
    }

    v2_printer.emit(
        Doc::new()
            .status(
                Role::Ok,
                format!("Encrypted {} via {}", file.display(), backend_name),
            )
            .with_data(serde_json::json!({
                "path": file.display().to_string(),
                "backend": backend_name,
            })),
    );

    Ok(())
}

pub fn cmd_secret_decrypt(cli: &Cli, v2_printer: &PrinterV2, file: &Path) -> anyhow::Result<()> {
    let backend = match get_secret_backend(cli, file) {
        Ok(b) => b,
        Err(e) => {
            v2_printer.emit(cfgd_core::output_v2::error_doc(
                &file.display().to_string(),
                "backend_unavailable",
                format!("{}", e),
                serde_json::json!({ "path": file.display().to_string() }),
            ));
            return Err(e);
        }
    };
    let backend_name = backend.name().to_string();

    let decrypted = match backend.decrypt_file(file) {
        Ok(d) => d,
        Err(e) => {
            v2_printer.emit(cfgd_core::output_v2::error_doc(
                &file.display().to_string(),
                "decryption_failed",
                format!("{}", e),
                serde_json::json!({
                    "path": file.display().to_string(),
                    "backend": backend_name,
                }),
            ));
            return Err(e.into());
        }
    };
    let plaintext = secrecy::ExposeSecret::expose_secret(&decrypted);

    // Plaintext must land on stdout so `cfgd secret decrypt foo.yaml > out.txt`
    // and `| pbcopy` work. `data_line` writes raw stdout without role decoration;
    // the status Doc routes to stderr (human) or the structured channel.
    v2_printer.data_line(plaintext);

    v2_printer.emit(
        Doc::new()
            .status(Role::Ok, format!("Decrypted {}", file.display()))
            .with_data(serde_json::json!({
                "path": file.display().to_string(),
                "backend": backend_name,
            })),
    );

    Ok(())
}

pub fn cmd_secret_edit(cli: &Cli, v2_printer: &PrinterV2, file: &Path) -> anyhow::Result<()> {
    let backend = match get_secret_backend(cli, file) {
        Ok(b) => b,
        Err(e) => {
            v2_printer.emit(cfgd_core::output_v2::error_doc(
                &file.display().to_string(),
                "backend_unavailable",
                format!("{}", e),
                serde_json::json!({ "path": file.display().to_string() }),
            ));
            return Err(e);
        }
    };
    let backend_name = backend.name().to_string();

    if let Err(e) = backend.edit_file(file) {
        v2_printer.emit(cfgd_core::output_v2::error_doc(
            &file.display().to_string(),
            "encryption_failed",
            format!("{}", e),
            serde_json::json!({
                "path": file.display().to_string(),
                "backend": backend_name,
            }),
        ));
        return Err(e.into());
    }

    v2_printer.emit(
        Doc::new()
            .status(
                Role::Ok,
                format!(
                    "Edited and re-encrypted {} via {}",
                    file.display(),
                    backend_name
                ),
            )
            .with_data(serde_json::json!({
                "path": file.display().to_string(),
                "backend": backend_name,
                "modified": true,
            })),
    );

    Ok(())
}

pub fn cmd_secret_init(cli: &Cli, v2_printer: &PrinterV2) -> anyhow::Result<()> {
    let config_dir = config_dir(cli);

    let sops_config_pre = config_dir.join(".sops.yaml");
    let default_dir = cfgd_core::default_config_dir();
    let age_key_pre = default_dir.join("age-key.txt");
    let already_initialized = age_key_pre.exists() && sops_config_pre.exists();

    let key_path = match secrets::init_age_key(&config_dir) {
        Ok(p) => p,
        Err(e) => {
            v2_printer.emit(cfgd_core::output_v2::error_doc(
                "age",
                "backend_unavailable",
                format!("{}", e),
                serde_json::json!({ "configDir": config_dir.display().to_string() }),
            ));
            return Err(e.into());
        }
    };

    if already_initialized {
        v2_printer.emit(
            Doc::new()
                .status(
                    Role::Info,
                    format!("Secrets already initialized at {}", key_path.display()),
                )
                .with_data(serde_json::json!({
                    "backend": "age",
                    "configPath": key_path.display().to_string(),
                    "sopsConfig": sops_config_pre.display().to_string(),
                    "alreadyInitialized": true,
                })),
        );
        return Ok(());
    }

    let sops_config = config_dir.join(".sops.yaml");
    let sops_path = if sops_config.exists() {
        Some(sops_config.display().to_string())
    } else {
        None
    };

    let init_sec = v2_printer.section("Secrets Initialized");
    let mut pairs: Vec<(String, String)> =
        vec![("Age key".to_string(), key_path.display().to_string())];
    if let Some(ref p) = sops_path {
        pairs.push((".sops.yaml".to_string(), p.clone()));
    }
    init_sec.kv_block(pairs);
    drop(init_sec);

    let mut payload = serde_json::json!({
        "backend": "age",
        "configPath": key_path.display().to_string(),
    });
    if let Some(ref p) = sops_path
        && let serde_json::Value::Object(map) = &mut payload
    {
        map.insert(
            "sopsConfig".to_string(),
            serde_json::Value::String(p.clone()),
        );
    }

    v2_printer.emit(
        Doc::new()
            .status(
                Role::Ok,
                "Secrets setup complete — files can now be encrypted with 'cfgd secret encrypt'",
            )
            .with_data(payload),
    );

    Ok(())
}
