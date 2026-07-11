use super::*;
use cfgd_core::PathDisplayExt;
use cfgd_core::output::{Doc, Printer, Role};

/// Pre-mutation snapshot of the profile's payload directory: the dir that
/// owns the payload (canonical `<name>/`, legacy `<name>/files/`) plus
/// whether it holds anything beyond the manifest. `None` when the dir does
/// not exist.
fn payload_dir_state(
    dir: &Path,
    manifest: Option<&Path>,
) -> anyhow::Result<Option<(std::path::PathBuf, bool)>> {
    if !dir.is_dir() {
        return Ok(None);
    }
    let mut has_payload = false;
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if manifest.is_some_and(|m| path == m) {
            continue;
        }
        has_payload = true;
        break;
    }
    Ok(Some((dir.to_path_buf(), has_payload)))
}

/// Remove the payload dir once the manifest is gone: an empty dir goes
/// silently, a payload-bearing dir goes only with the consent gathered up
/// front, otherwise it is kept and noted.
fn cleanup_payload_dir(
    printer: &Printer,
    dir: &Path,
    has_payload: bool,
    remove_payload: bool,
) -> anyhow::Result<()> {
    if !has_payload {
        std::fs::remove_dir(dir)?;
    } else if remove_payload {
        std::fs::remove_dir_all(dir)?;
    } else {
        printer.status_simple(Role::Info, format!("Kept {}", dir.posix()));
    }
    Ok(())
}

pub fn cmd_profile_delete(
    cli: &Cli,
    printer: &Printer,
    name: &str,
    yes: bool,
    ignore_not_found: bool,
) -> anyhow::Result<()> {
    validate_resource_name(name, "Profile")?;
    printer.heading(format!("Delete Profile: {}", name));

    let pdir = profiles_dir(cli);
    let profile_path = match cfgd_core::config::find_profile_path(&pdir, name) {
        Ok(p) => p,
        Err(cfgd_core::errors::ConfigError::ProfileNotFound { .. }) if ignore_not_found => {
            return crate::cli::emit_not_found_ignored(printer, "profile", name);
        }
        // Typed not-found routing (→ exit 6). The active-profile guard below
        // stays exit 1 — it is a precondition failure, not a not-found.
        Err(e) => return Err(profile_lookup_error(e, name)),
    };
    let is_canonical = profile_path == cfgd_core::config::canonical_profile_path(&pdir, name);

    // Safety: refuse if active profile
    if cli.config.exists()
        && let Ok(cfg) = config::load_config(&cli.config)
        && cfg.spec.profile.as_deref() == Some(name)
    {
        return Err(crate::cli::cli_error(
            name,
            "active_profile",
            format!(
                "Cannot delete '{}' — it is the active profile. Switch first with: cfgd profile switch <other>",
                name
            ),
            serde_json::json!({}),
        ));
    }

    // Safety: refuse if inherited by other profiles
    let inheritors = profiles_inheriting(&pdir, name, printer)?;
    if !inheritors.is_empty() {
        return Err(crate::cli::cli_error(
            name,
            "inherited",
            format!(
                "Cannot delete '{}' — inherited by: {}",
                name,
                inheritors.join(", ")
            ),
            serde_json::json!({ "inheritors": inheritors }),
        ));
    }

    // Snapshot payload state and gather EVERY confirmation before ANY
    // mutation — an abort (Ctrl-C/EOF) at the second prompt must leave the
    // manifest intact, not report a half-finished delete as a failure.
    // `<name>/` belongs to the profile in both layouts: canonical keeps
    // profile.yaml inside it, legacy keeps files/.
    let profile_dir = pdir.join(name);
    let payload = if is_canonical {
        payload_dir_state(&profile_dir, Some(&profile_path))?
    } else {
        // The payload is the same kind of thing in both layouts, so a
        // non-empty legacy files/ dir gets the same prompt gate as a
        // payload-bearing canonical dir; empty/absent stays silent cleanup.
        payload_dir_state(&profile_dir.join("files"), None)?
    };

    if !yes && !printer.prompt_confirm(&format!("Delete profile '{}'?", name))? {
        printer.emit(
            Doc::new()
                .status(Role::Info, "Cancelled")
                .with_data(serde_json::json!({
                    "name": name,
                    "cancelled": true,
                })),
        );
        return Ok(());
    }

    let remove_payload = match &payload {
        Some((dir, true)) => {
            yes || printer.prompt_confirm(&format!(
                "Profile directory '{}' still contains payload files — remove it too?",
                dir.posix()
            ))?
        }
        _ => false,
    };

    std::fs::remove_file(&profile_path)?;

    if let Some((dir, has_payload)) = payload {
        cleanup_payload_dir(printer, &dir, has_payload, remove_payload)?;
    }
    // Legacy layout only: files/ was handled above, so the now-possibly-empty
    // parent `<name>/` gets a silent cleanup too. The asymmetry is
    // intentional — in the canonical layout the parent IS the payload dir.
    if !is_canonical && profile_dir.is_dir() && std::fs::read_dir(&profile_dir)?.next().is_none() {
        std::fs::remove_dir(&profile_dir)?;
    }

    printer.emit(
        Doc::new()
            .status(Role::Ok, format!("Deleted profile '{}'", name))
            .with_data(serde_json::json!({
                "name": name,
                "cancelled": false,
            })),
    );

    update_workflow_best_effort(cli, printer);

    Ok(())
}
