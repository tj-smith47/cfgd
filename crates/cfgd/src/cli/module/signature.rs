use super::*;
use cfgd_core::output::{Printer, Role};

/// Cryptographically verify a git tag signature using `git tag -v`.
/// Returns `Ok(true)` if verified, `Ok(false)` if verification fails (bad sig),
/// or `Err` if `git` is not available or keyring is not configured.
fn verify_tag_signature_cryptographic(repo_dir: &Path, tag_name: &str) -> anyhow::Result<bool> {
    let output = cfgd_core::git_cmd_local()
        .args(["tag", "-v", tag_name])
        .current_dir(repo_dir)
        .output()?;

    if output.status.success() {
        Ok(true)
    } else {
        let stderr = cfgd_core::stderr_lossy_trimmed(&output);
        // Distinguish "bad signature" from "no keyring / gpg not found"
        if stderr.contains("BAD signature")
            || stderr.contains("BADSIG")
            || stderr.contains("verification failed")
        {
            Ok(false) // Signature present but invalid
        } else {
            // gpg not installed, key not in keyring, etc.
            anyhow::bail!("{}", stderr)
        }
    }
}

/// Check tag signature and enforce require-signatures policy.
/// Loads `require_signatures` from config, checks the tag signature,
/// and bails if policy is violated. Returns Ok(()) if allowed to proceed.
pub(crate) fn enforce_signature_policy(
    cli: &Cli,
    v2_printer: &Printer,
    tag: Option<&str>,
    module_name: &str,
    allow_unsigned: bool,
    cache_base: &Path,
    repo_url: &str,
) -> anyhow::Result<()> {
    let require_signatures = cli
        .config
        .exists()
        .then(|| config::load_config(&cli.config).ok())
        .flatten()
        .and_then(|c| c.spec.modules)
        .and_then(|m| m.security)
        .is_some_and(|s| s.require_signatures);

    let Some(tag) = tag else {
        if require_signatures && !allow_unsigned {
            v2_printer.emit(cfgd_core::output::error_doc(
                module_name,
                "signature_required",
                format!(
                    "Module '{}' has no tag — your config has require-signatures enabled. Pass --allow-unsigned to override.",
                    module_name
                ),
                serde_json::json!({}),
            ));
            anyhow::bail!(
                "Module '{}' has no tag — your config has require-signatures enabled. Pass --allow-unsigned to override.",
                module_name
            );
        }
        return Ok(());
    };

    let repo_dir = modules::git_cache_dir(cache_base, repo_url);
    let sig_status = modules::check_tag_signature(&repo_dir, tag, module_name);

    match &sig_status {
        Ok(modules::TagSignatureStatus::SignaturePresent) => {
            match verify_tag_signature_cryptographic(&repo_dir, tag) {
                Ok(true) => {
                    v2_printer.status_simple(Role::Ok, format!("Tag '{}' signature verified", tag))
                }
                Ok(false) => {
                    v2_printer.emit(cfgd_core::output::error_doc(
                        module_name,
                        "signature_failed",
                        format!(
                            "Tag '{}' has an INVALID signature — refusing to proceed",
                            tag
                        ),
                        serde_json::json!({ "tag": tag }),
                    ));
                    anyhow::bail!(
                        "Signature verification failed for tag '{}' — the signature is present but invalid",
                        tag
                    );
                }
                Err(e) => {
                    v2_printer.status_simple(
                        Role::Warn,
                        format!(
                            "Tag '{}' has a signature but verification skipped: {}",
                            tag, e
                        ),
                    );
                }
            }
        }
        Ok(modules::TagSignatureStatus::Unsigned)
        | Ok(modules::TagSignatureStatus::LightweightTag) => {
            let label = match &sig_status {
                Ok(modules::TagSignatureStatus::LightweightTag) => {
                    format!("Tag '{}' is lightweight (cannot carry a signature)", tag)
                }
                _ => format!("Tag '{}' is unsigned", tag),
            };
            if require_signatures && !allow_unsigned {
                v2_printer.emit(cfgd_core::output::error_doc(
                    module_name,
                    "signature_required",
                    label.clone(),
                    serde_json::json!({ "tag": tag }),
                ));
                anyhow::bail!(
                    "Module '{}' tag '{}' is unsigned — your config has require-signatures enabled. Pass --allow-unsigned to override.",
                    module_name,
                    tag
                );
            }
            v2_printer.status_simple(Role::Info, label);
        }
        Ok(modules::TagSignatureStatus::TagNotFound) => {
            v2_printer.status_simple(Role::Warn, format!("Tag '{}' not found in repo", tag));
        }
        Err(e) => v2_printer.status_simple(Role::Warn, format!("Signature check: {}", e)),
    }

    Ok(())
}
