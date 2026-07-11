use super::*;
use cfgd_core::output::{Doc, Printer, Role, renderer::Table};

pub fn cmd_source_remove(
    cli: &Cli,
    printer: &Printer,
    name: &str,
    keep_all: bool,
    remove_all: bool,
    ignore_not_found: bool,
) -> anyhow::Result<()> {
    if keep_all && remove_all {
        return Err(crate::cli::cli_error(
            name,
            "conflicting_flags",
            "cannot use --keep-all and --remove-all together",
            serde_json::json!({}),
        ));
    }

    printer.heading(format!("Remove Source: {}", name));

    let config_path = cli.config.clone();
    let cfg = config::load_config(&config_path)?;

    if !cfg.spec.sources.iter().any(|s| s.name == name) {
        if ignore_not_found {
            return crate::cli::emit_not_found_ignored(printer, "source", name);
        }
        // Carry the typed SourceError::NotFound so the exit-code downcast resolves
        // to ExitCode::NotFound (6), uniform with every other named-resource miss.
        return Err(crate::cli::cli_error_ctx(
            cfgd_core::errors::CfgdError::Source(cfgd_core::errors::SourceError::NotFound {
                name: name.to_string(),
            })
            .into(),
            name,
            "not_found",
            format!("Source '{}' not found in config", name),
            serde_json::json!({}),
        ));
    }

    let state = open_state_store(cli.state_dir.as_deref())?;
    let resources = state.managed_resources_by_source(name)?;

    let mut disposition = "removed";
    let mut managed_count = resources.len();

    if !resources.is_empty() && !keep_all && !remove_all {
        // Interactive: Keep / Remove / Cancel
        {
            let res_sec = printer.section(format!(
                "This source manages {} resource(s)",
                resources.len()
            ));
            let mut t = Table::new(["Type", "Resource"]);
            for r in &resources {
                t = t.row([r.resource_type.clone(), r.resource_id.clone()]);
            }
            res_sec.table(t);
        }

        let options = vec![
            "Keep all (resources become locally managed)".to_string(),
            "Remove all".to_string(),
            "Cancel (abort remove)".to_string(),
        ];
        let choice = printer.prompt_select("What to do with these resources?", &options)?;

        if choice.starts_with("Cancel") {
            printer.emit(
                Doc::new()
                    .status(Role::Info, "Cancelled — source not removed")
                    .with_data(serde_json::json!({
                        "name": name,
                        "managedResources": managed_count,
                        "cancelled": true,
                    })),
            );
            return Ok(());
        }

        if choice.starts_with("Keep") {
            // Re-assign resources to local
            for r in &resources {
                state.upsert_managed_resource(
                    &r.resource_type,
                    &r.resource_id,
                    "local",
                    r.last_hash.as_deref(),
                    r.last_applied,
                )?;
            }
            printer.status_simple(Role::Info, "Resources transferred to local management");
            disposition = "kept";
        } else {
            disposition = "purged";
        }
    } else if keep_all {
        for r in &resources {
            state.upsert_managed_resource(
                &r.resource_type,
                &r.resource_id,
                "local",
                r.last_hash.as_deref(),
                r.last_applied,
            )?;
        }
        disposition = "kept";
    } else if remove_all {
        disposition = "purged";
    } else {
        // No managed resources — neutral disposition
        managed_count = 0;
    }

    // Purged resources must be deleted from state, not merely relabeled —
    // otherwise the rows linger orphaned under the now-removed source name
    // (their `source` column still points at a config_source that no longer
    // exists). "kept" already re-owns them to `local` above.
    if disposition == "purged" {
        for r in &resources {
            state.remove_managed_resource(&r.resource_type, &r.resource_id)?;
        }
    }

    // Remove from config
    remove_source_from_config(&config_path, name)?;

    // Remove from state
    state.remove_config_source(name)?;
    state.remove_source_config_hash(name)?;

    // Remove the entry from sources.lock (best-effort — a missing entry is fine).
    let cfg_dir = config_dir(cli);
    if let Err(e) = cfgd_core::remove_source_lock_entry(&cfg_dir, name) {
        printer.status_simple(Role::Warn, super::sources_lock_update_warning(&e));
    }

    // Remove the cached clone. It lives at `<cache_dir>/<name>` (see
    // SourceManager::load_source). Delete the directory directly: the previous
    // SourceManager::remove_source path keyed off an in-memory `sources` map
    // that this command never populates, so it returned NotFound and the cache
    // dir leaked on every removal.
    let cached_dir = source_cache_dir(cli)?.join(name);
    if cached_dir.exists()
        && let Err(e) = std::fs::remove_dir_all(&cached_dir)
    {
        // Config + state mutations already landed, so a cache-removal failure
        // is non-fatal — but surface it as a visible warning rather than a
        // swallowed debug log, so a stuck cache dir doesn't go unnoticed.
        printer.status_simple(
            Role::Warn,
            format!(
                "Could not remove cached data for '{}': {}",
                name,
                cfgd_core::output::collapse_to_subject_line(&e)
            ),
        );
    }

    printer.emit(
        Doc::new()
            .status(Role::Ok, format!("Source '{}' removed", name))
            .with_data(serde_json::json!({
                "name": name,
                "managedResources": managed_count,
                "disposition": disposition,
                "cancelled": false,
            })),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cfgd_core::output::{OutputFormat, PromptAnswer};

    const SEED_CONFIG: &str = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n  sources:\n    - name: acme\n      origin:\n        type: Git\n        url: https://example.com/acme/dev.git\n        branch: main\n";

    /// Build a Cli whose config + state dirs both live under `dir`, with a
    /// seeded config containing one source named "acme".
    fn cli_with_seeded_config(dir: &std::path::Path) -> Cli {
        let config_path = dir.join("cfgd.yaml");
        std::fs::write(&config_path, SEED_CONFIG).expect("write seed config");
        let state_dir = dir.join("state");
        std::fs::create_dir_all(&state_dir).expect("mk state dir");
        Cli {
            config: config_path,
            config_explicit: false,
            profile: None,
            verbose: 0,
            quiet: true,
            no_color: true,
            output: crate::cli::OutputFormatArg(OutputFormat::Table),
            list_envelope: false,
            jsonpath: None,
            state_dir: Some(state_dir),
            config_dir: None,
            cache_dir: None,
            runtime_dir: None,
            scope_arg: crate::cli::ScopeArg::User,
            command: None,
        }
    }

    fn config_has_source(cli: &Cli, name: &str) -> bool {
        let cfg = config::load_config(&cli.config).expect("reload config");
        cfg.spec.sources.iter().any(|s| s.name == name)
    }

    #[test]
    fn remove_rejects_keep_all_and_remove_all_together() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cli = cli_with_seeded_config(dir.path());
        let (printer, _cap) = Printer::for_test_doc();

        let err = cmd_source_remove(&cli, &printer, "acme", true, true, false)
            .expect_err("keep_all + remove_all must conflict");
        drop(printer);

        let meta = err
            .downcast_ref::<crate::cli::CliErrorMeta>()
            .expect("must return CliErrorMeta");
        assert_eq!(meta.error_kind, "conflicting_flags", "got: {meta:?}");
    }

    #[test]
    fn remove_errors_when_source_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cli = cli_with_seeded_config(dir.path());
        let (printer, _cap) = Printer::for_test_doc();

        let err = cmd_source_remove(&cli, &printer, "nope", false, false, false)
            .expect_err("absent source must error");
        drop(printer);

        let meta = err
            .downcast_ref::<crate::cli::CliErrorMeta>()
            .expect("must return CliErrorMeta");
        assert_eq!(meta.error_kind, "not_found", "got: {meta:?}");
        assert_eq!(meta.name, "nope");
    }

    #[test]
    fn remove_absent_source_with_ignore_not_found_emits_noop_doc() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cli = cli_with_seeded_config(dir.path());
        let (printer, cap) = Printer::for_test_doc();

        cmd_source_remove(&cli, &printer, "nope", false, false, true)
            .expect("--ignore-not-found must succeed for an absent source");
        drop(printer);

        let doc = cap.json().expect("no-op removal must emit a Doc");
        assert_eq!(doc["name"], "nope");
        assert_eq!(doc["kind"], "source");
        assert_eq!(doc["removed"], false);
        assert_eq!(doc["reason"], "not_found");
    }

    #[test]
    fn remove_clean_source_reports_neutral_disposition_and_mutates_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cli = cli_with_seeded_config(dir.path());
        assert!(
            config_has_source(&cli, "acme"),
            "precondition: source present"
        );
        let (printer, cap) = Printer::for_test_doc();

        cmd_source_remove(&cli, &printer, "acme", false, false, false)
            .expect("removing a source with no managed resources must succeed");
        drop(printer);

        let doc = cap.json().expect("removal must emit a Doc");
        assert_eq!(doc["name"], "acme");
        assert_eq!(doc["managedResources"], 0, "no managed resources → count 0");
        assert_eq!(
            doc["disposition"], "removed",
            "no-resource removal is the neutral 'removed' disposition"
        );
        assert_eq!(doc["cancelled"], false);

        assert!(
            !config_has_source(&cli, "acme"),
            "source must be gone from on-disk config after removal"
        );
    }

    #[test]
    fn remove_with_keep_all_transfers_resources_to_local() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cli = cli_with_seeded_config(dir.path());

        // Seed state: one managed resource owned by source "acme".
        let state = open_state_store(cli.state_dir.as_deref()).expect("open state");
        state
            .upsert_config_source(
                "acme",
                "https://example.com/acme/dev.git",
                "main",
                None,
                None,
                None,
            )
            .expect("seed config_source");
        state
            .upsert_managed_resource("file", "/etc/foo", "acme", None, None)
            .expect("seed managed resource");
        drop(state);

        let (printer, cap) = Printer::for_test_doc();
        cmd_source_remove(&cli, &printer, "acme", true, false, false)
            .expect("keep_all removal must succeed");
        drop(printer);

        let doc = cap.json().expect("removal must emit a Doc");
        assert_eq!(
            doc["disposition"], "kept",
            "--keep-all must report 'kept' disposition"
        );
        assert_eq!(
            doc["managedResources"], 1,
            "managed resource count must reflect the one seeded resource"
        );

        // The resource must now be re-owned by "local", not "acme".
        let state = open_state_store(cli.state_dir.as_deref()).expect("reopen state");
        let res = state.managed_resources().expect("list resources");
        let foo = res
            .iter()
            .find(|r| r.resource_id == "/etc/foo")
            .expect("resource must still exist after keep-all");
        assert_eq!(
            foo.source, "local",
            "keep-all must transfer the resource to local ownership"
        );
        assert!(
            state
                .managed_resources_by_source("acme")
                .expect("query by source")
                .is_empty(),
            "no resource may remain owned by the removed source"
        );
    }

    #[test]
    fn remove_with_remove_all_reports_purged_disposition() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cli = cli_with_seeded_config(dir.path());

        let state = open_state_store(cli.state_dir.as_deref()).expect("open state");
        state
            .upsert_config_source(
                "acme",
                "https://example.com/acme/dev.git",
                "main",
                None,
                None,
                None,
            )
            .expect("seed config_source");
        state
            .upsert_managed_resource("file", "/etc/bar", "acme", None, None)
            .expect("seed managed resource");
        drop(state);

        let (printer, cap) = Printer::for_test_doc();
        cmd_source_remove(&cli, &printer, "acme", false, true, false)
            .expect("remove_all removal must succeed");
        drop(printer);

        let doc = cap.json().expect("removal must emit a Doc");
        assert_eq!(
            doc["disposition"], "purged",
            "--remove-all must report 'purged' disposition"
        );
        assert_eq!(doc["managedResources"], 1);

        // The config_source row is removed.
        let state = open_state_store(cli.state_dir.as_deref()).expect("reopen state");
        assert!(
            state
                .config_sources()
                .expect("list config sources")
                .iter()
                .all(|s| s.name != "acme"),
            "remove must delete the config_source row for the source"
        );
        // --remove-all ("purged") must delete the managed_resource rows owned by
        // the source, not leave them orphaned under a config_source that no
        // longer exists.
        let still_owned = state
            .managed_resources_by_source("acme")
            .expect("query by source");
        assert!(
            still_owned.is_empty(),
            "--remove-all must purge the managed_resource rows, leaving none owned by the removed source"
        );
        // And nothing should have been silently re-homed to local either.
        assert!(
            state
                .managed_resources_by_source("local")
                .expect("query local")
                .iter()
                .all(|r| r.resource_id != "/etc/bar"),
            "purged resources must not reappear under local management"
        );
    }

    #[test]
    fn remove_interactive_cancel_leaves_source_intact() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cli = cli_with_seeded_config(dir.path());

        let state = open_state_store(cli.state_dir.as_deref()).expect("open state");
        state
            .upsert_config_source(
                "acme",
                "https://example.com/acme/dev.git",
                "main",
                None,
                None,
                None,
            )
            .expect("seed config_source");
        state
            .upsert_managed_resource("file", "/etc/baz", "acme", None, None)
            .expect("seed managed resource");
        drop(state);

        // Seed the interactive select to choose the "Cancel" option.
        let (printer, cap) =
            Printer::for_test_doc_with_prompt_responses(vec![PromptAnswer::Select(
                "Cancel (abort remove)".into(),
            )]);
        cmd_source_remove(&cli, &printer, "acme", false, false, false)
            .expect("cancel path must return Ok");
        drop(printer);

        let doc = cap.json().expect("cancel must emit a Doc");
        assert_eq!(doc["cancelled"], true, "cancel must set cancelled=true");
        assert_eq!(doc["managedResources"], 1);

        assert!(
            config_has_source(&cli, "acme"),
            "cancel must leave the source in config untouched"
        );
    }

    #[test]
    fn remove_interactive_keep_choice_transfers_to_local() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cli = cli_with_seeded_config(dir.path());

        let state = open_state_store(cli.state_dir.as_deref()).expect("open state");
        state
            .upsert_config_source(
                "acme",
                "https://example.com/acme/dev.git",
                "main",
                None,
                None,
                None,
            )
            .expect("seed config_source");
        state
            .upsert_managed_resource("file", "/etc/keepme", "acme", None, None)
            .expect("seed managed resource");
        drop(state);

        let (printer, cap) =
            Printer::for_test_doc_with_prompt_responses(vec![PromptAnswer::Select(
                "Keep all (resources become locally managed)".into(),
            )]);
        cmd_source_remove(&cli, &printer, "acme", false, false, false)
            .expect("keep choice must return Ok");
        drop(printer);

        let doc = cap.json().expect("keep must emit a Doc");
        assert_eq!(doc["disposition"], "kept");
        assert_eq!(doc["cancelled"], false);
        assert!(
            !config_has_source(&cli, "acme"),
            "keep still removes the source from config (resources transferred, not the subscription)"
        );

        let state = open_state_store(cli.state_dir.as_deref()).expect("reopen state");
        let res = state.managed_resources().expect("list");
        let r = res
            .iter()
            .find(|r| r.resource_id == "/etc/keepme")
            .expect("resource retained");
        assert_eq!(r.source, "local", "keep choice transfers resource to local");
    }
}
