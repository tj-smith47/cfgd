//! `cfgd <kind> validate <file|->` — validate a single resource document.
//!
//! One shared [`run_validate`] backs every author-facing kind. It reads YAML
//! from a file path (or stdin when `source == "-"`), guards that the document's
//! declared `kind:` matches the noun the user invoked, runs the unified
//! [`cfgd_core::generate::validate::validate_document`] validator, and reports
//! the `{kind, valid, errors}` result.
//!
//! Channels of truth:
//! - **valid** → a success `Doc` carrying the structured payload (exit 0);
//! - **invalid** → an [`crate::cli::error::CliErrorMeta`]-carrying error whose
//!   `extras` IS the same `{kind, valid, errors}` payload, wrapped over a
//!   [`cfgd_core::errors::ConfigError::Invalid`] so the central sink renders one
//!   payload and the process exits with the config-invalid code (4). Emitting
//!   the result Doc here AND returning `Err` would double-emit under `-o json`.

use std::io::Read;

use cfgd_core::output::{Doc, Printer, Role};

use crate::cli::cli_error_ctx;

/// Read the YAML document for `source`: a file path, or `-` for stdin.
fn read_source(source: &str) -> anyhow::Result<String> {
    if source == "-" {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| anyhow::anyhow!("failed to read document from stdin: {e}"))?;
        Ok(buf)
    } else {
        std::fs::read_to_string(source)
            .map_err(|e| anyhow::anyhow!("failed to read document '{source}': {e}"))
    }
}

/// Validate a Module document (`cfgd module validate <file|->`).
pub fn cmd_module_validate(printer: &Printer, source: &str) -> anyhow::Result<()> {
    run_validate(printer, "Module", source)
}

/// Validate a Profile document (`cfgd profile validate <file|->`).
pub fn cmd_profile_validate(printer: &Printer, source: &str) -> anyhow::Result<()> {
    run_validate(printer, "Profile", source)
}

/// Validate a ConfigSource document (`cfgd source validate <file|->`).
pub fn cmd_source_validate(printer: &Printer, source: &str) -> anyhow::Result<()> {
    run_validate(printer, "ConfigSource", source)
}

/// Validate a MachineConfig document (`cfgd machineconfig validate <file|->`).
pub fn cmd_machineconfig_validate(printer: &Printer, source: &str) -> anyhow::Result<()> {
    run_validate(printer, "MachineConfig", source)
}

/// Validate a ConfigPolicy document (`cfgd configpolicy validate <file|->`).
pub fn cmd_configpolicy_validate(printer: &Printer, source: &str) -> anyhow::Result<()> {
    run_validate(printer, "ConfigPolicy", source)
}

/// Validate a ClusterConfigPolicy document (`cfgd clusterconfigpolicy validate <file|->`).
pub fn cmd_clusterconfigpolicy_validate(printer: &Printer, source: &str) -> anyhow::Result<()> {
    run_validate(printer, "ClusterConfigPolicy", source)
}

/// Validate a single resource document of `expected_kind` read from `source`.
///
/// Emits a success `Doc` carrying `{kind, valid: true, errors: []}` on success.
/// On a kind mismatch or a validation failure, returns an error carrying the
/// `{kind, valid: false, errors: [...]}` payload as structured `extras` so the
/// central error sink renders exactly one structured payload; the process exits
/// with the config-invalid code.
pub fn run_validate(printer: &Printer, expected_kind: &str, source: &str) -> anyhow::Result<()> {
    let yaml = read_source(source)?;

    // DX guard: name BOTH kinds when the declared `kind:` disagrees with the
    // noun the user invoked, before delegating to the shared validator.
    if let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(&yaml)
        && let Some(found) = value.get("kind").and_then(|v| v.as_str())
        && found != expected_kind
    {
        let message = format!("Expected kind '{expected_kind}', found '{found}'");
        return Err(invalid_doc_error(
            expected_kind,
            std::slice::from_ref(&message),
            message.clone(),
        ));
    }

    let result = cfgd_core::generate::validate::validate_document(&yaml);

    if result.valid {
        printer.emit(
            Doc::new()
                .status(Role::Ok, format!("{expected_kind} document is valid"))
                .with_data(validation_payload(expected_kind, true, &[])),
        );
        return Ok(());
    }

    let message = format!(
        "{expected_kind} document is invalid: {}",
        result.errors.join("; ")
    );
    Err(invalid_doc_error(expected_kind, &result.errors, message))
}

/// The structured `{kind, valid, errors}` payload shared by the success Doc and
/// the failure `extras` so consumers see one shape on either channel.
fn validation_payload(kind: &str, valid: bool, errors: &[String]) -> serde_json::Value {
    serde_json::json!({
        "kind": kind,
        "valid": valid,
        "errors": errors,
    })
}

/// Build the invalid-document error: a `CliErrorMeta` carrying the validation
/// payload as `extras`, wrapped over a typed `ConfigError::Invalid` so the exit
/// code resolves to config-invalid (4) through the central sink's downcast.
fn invalid_doc_error(kind: &str, errors: &[String], message: String) -> anyhow::Error {
    let source: anyhow::Error =
        cfgd_core::errors::CfgdError::Config(cfgd_core::errors::ConfigError::Invalid {
            message: message.clone(),
        })
        .into();
    cli_error_ctx(
        source,
        kind,
        "validation_failed",
        message,
        validation_payload(kind, false, errors),
    )
}
