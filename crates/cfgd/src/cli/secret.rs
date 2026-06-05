use super::*;
use cfgd_core::PathDisplayExt;
use cfgd_core::output::{Doc, Printer, Role};

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").to_string()
}

pub fn cmd_secret_encrypt(cli: &Cli, printer: &Printer, file: &Path) -> anyhow::Result<()> {
    let backend = match get_secret_backend(cli, file) {
        Ok(b) => b,
        Err(e) => {
            let full = format!("{}", e);
            return Err(crate::cli::cli_error_ctx(
                e,
                file.display().to_string(),
                "backend_unavailable",
                first_line(&full),
                serde_json::json!({ "path": cfgd_core::to_posix_string(file), "detail": full }),
            ));
        }
    };
    let backend_name = backend.name().to_string();

    if let Err(e) = backend.encrypt_file(file) {
        let full = format!("{}", e);
        return Err(crate::cli::cli_error_ctx(
            e.into(),
            file.display().to_string(),
            "encryption_failed",
            first_line(&full),
            serde_json::json!({
                "path": cfgd_core::to_posix_string(file),
                "backend": backend_name,
                "detail": full,
            }),
        ));
    }

    printer.emit(
        Doc::new()
            .status(
                Role::Ok,
                format!("Encrypted {} via {}", file.posix(), backend_name),
            )
            .with_data(serde_json::json!({
                "path": cfgd_core::to_posix_string(file),
                "backend": backend_name,
            })),
    );

    Ok(())
}

pub fn cmd_secret_decrypt(cli: &Cli, printer: &Printer, file: &Path) -> anyhow::Result<()> {
    let backend = match get_secret_backend(cli, file) {
        Ok(b) => b,
        Err(e) => {
            let full = format!("{}", e);
            return Err(crate::cli::cli_error_ctx(
                e,
                file.display().to_string(),
                "backend_unavailable",
                first_line(&full),
                serde_json::json!({ "path": cfgd_core::to_posix_string(file), "detail": full }),
            ));
        }
    };
    let backend_name = backend.name().to_string();

    let decrypted = match backend.decrypt_file(file) {
        Ok(d) => d,
        Err(e) => {
            let full = format!("{}", e);
            return Err(crate::cli::cli_error_ctx(
                e.into(),
                file.display().to_string(),
                "decryption_failed",
                first_line(&full),
                serde_json::json!({
                    "path": cfgd_core::to_posix_string(file),
                    "backend": backend_name,
                    "detail": full,
                }),
            ));
        }
    };
    let plaintext = secrecy::ExposeSecret::expose_secret(&decrypted);

    // Plaintext must land on stdout so `cfgd secret decrypt foo.yaml > out.txt`
    // and `| pbcopy` work in human mode. Under structured output (`-o json`),
    // skip the raw stdout sink so plaintext doesn't contaminate both the
    // JSON channel and raw stdout — the structured caller receives plaintext
    // inside the Doc payload.
    if printer.is_structured() {
        printer.emit(
            Doc::new()
                .status(Role::Ok, format!("Decrypted {}", file.posix()))
                .with_data(serde_json::json!({
                    "path": cfgd_core::to_posix_string(file),
                    "backend": backend_name,
                    "plaintext": plaintext,
                })),
        );
        return Ok(());
    }

    printer.data_line(plaintext);

    printer.emit(
        Doc::new()
            .status(Role::Ok, format!("Decrypted {}", file.posix()))
            .with_data(serde_json::json!({
                "path": cfgd_core::to_posix_string(file),
                "backend": backend_name,
            })),
    );

    Ok(())
}

pub fn cmd_secret_edit(cli: &Cli, printer: &Printer, file: &Path) -> anyhow::Result<()> {
    let backend = match get_secret_backend(cli, file) {
        Ok(b) => b,
        Err(e) => {
            let full = format!("{}", e);
            return Err(crate::cli::cli_error_ctx(
                e,
                file.display().to_string(),
                "backend_unavailable",
                first_line(&full),
                serde_json::json!({ "path": cfgd_core::to_posix_string(file), "detail": full }),
            ));
        }
    };
    let backend_name = backend.name().to_string();

    if let Err(e) = backend.edit_file(file) {
        let full = format!("{}", e);
        return Err(crate::cli::cli_error_ctx(
            e.into(),
            file.display().to_string(),
            "edit_failed",
            first_line(&full),
            serde_json::json!({
                "path": cfgd_core::to_posix_string(file),
                "backend": backend_name,
                "detail": full,
            }),
        ));
    }

    printer.emit(
        Doc::new()
            .status(
                Role::Ok,
                format!(
                    "Edited and re-encrypted {} via {}",
                    file.posix(),
                    backend_name
                ),
            )
            .with_data(serde_json::json!({
                "path": cfgd_core::to_posix_string(file),
                "backend": backend_name,
                "modified": true,
            })),
    );

    Ok(())
}

pub fn cmd_secret_init(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    let config_dir = config_dir(cli);

    let sops_config_pre = config_dir.join(".sops.yaml");
    let default_dir = cfgd_core::default_config_dir();
    let age_key_pre = default_dir.join("age-key.txt");
    let already_initialized = age_key_pre.exists() && sops_config_pre.exists();

    let key_path = match secrets::init_age_key(&config_dir) {
        Ok(p) => p,
        Err(e) => {
            let full = format!("{}", e);
            return Err(crate::cli::cli_error_ctx(
                e.into(),
                "age",
                "backend_unavailable",
                first_line(&full),
                serde_json::json!({
                    "configDir": cfgd_core::to_posix_string(&config_dir),
                    "detail": full,
                }),
            ));
        }
    };

    if already_initialized {
        printer.emit(
            Doc::new()
                .status(
                    Role::Info,
                    format!("Secrets already initialized at {}", key_path.posix()),
                )
                .with_data(serde_json::json!({
                    "backend": "age",
                    "configPath": cfgd_core::to_posix_string(&key_path),
                    "sopsConfig": cfgd_core::to_posix_string(&sops_config_pre),
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

    let init_sec = printer.section("Secrets Initialized");
    let mut pairs: Vec<(String, String)> = vec![("Age key".to_string(), key_path.display_posix())];
    if let Some(ref p) = sops_path {
        pairs.push((".sops.yaml".to_string(), p.clone()));
    }
    init_sec.kv_block(pairs);
    drop(init_sec);

    let mut payload = serde_json::json!({
        "backend": "age",
        "configPath": cfgd_core::to_posix_string(&key_path),
    });
    if let Some(ref p) = sops_path
        && let serde_json::Value::Object(map) = &mut payload
    {
        map.insert(
            "sopsConfig".to_string(),
            serde_json::Value::String(p.clone()),
        );
    }

    printer.emit(
        Doc::new()
            .status(
                Role::Ok,
                "Secrets setup complete — files can now be encrypted with 'cfgd secret encrypt'",
            )
            .with_data(payload),
    );

    Ok(())
}
