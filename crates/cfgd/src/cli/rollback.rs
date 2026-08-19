use super::*;

use cfgd_core::output::{Doc, Printer, Role, condense_script_label};

pub fn cmd_rollback(
    printer: &Printer,
    apply_id: i64,
    yes: bool,
    state_dir: Option<&Path>,
    scope: cfgd_core::Scope,
) -> anyhow::Result<()> {
    let state = open_state_store(state_dir, scope)?;

    if state.get_apply(apply_id)?.is_none() {
        // Typed StateError::ApplyNotFound so the exit-code downcast resolves to
        // ExitCode::NotFound (6), uniform with every other named-resource miss
        // (backup name, snapshot, module, profile) and so `-o json` gets the
        // stable `{"error":"not_found",...}` payload instead of nothing.
        let e = cfgd_core::errors::CfgdError::State(cfgd_core::errors::StateError::ApplyNotFound {
            apply_id,
        });
        let message = e.to_string();
        return Err(crate::cli::cli_error_ctx(
            e.into(),
            apply_id.to_string(),
            "not_found",
            message,
            serde_json::json!({}),
        ));
    }

    let target_backups = state.get_apply_backups(apply_id)?;
    let after_backups = state.file_backups_after_apply(apply_id)?;
    let after_entries = state.journal_entries_after_apply(apply_id)?;

    let mut file_paths = std::collections::HashSet::new();
    for bk in &target_backups {
        file_paths.insert(bk.file_path.clone());
    }
    for bk in &after_backups {
        file_paths.insert(bk.file_path.clone());
    }
    let file_count = file_paths.len();
    let non_file_actions: Vec<String> = after_entries
        .iter()
        .filter(|e| !e.is_file_work())
        .map(|e| e.resource_id.clone())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    let non_file_count = non_file_actions.len();

    printer.heading("Rollback");
    let mut kv_pairs: Vec<(String, String)> = vec![
        ("Target apply ID".to_string(), apply_id.to_string()),
        (
            "File backups to restore".to_string(),
            file_count.to_string(),
        ),
    ];
    if non_file_count > 0 {
        kv_pairs.push(("Non-file actions".to_string(), non_file_count.to_string()));
    }
    printer.kv_block(kv_pairs);

    if file_count == 0 && non_file_count == 0 {
        printer.emit(
            Doc::new()
                .status(
                    Role::Info,
                    "No subsequent changes to roll back — system is already at this apply",
                )
                .with_data(&RollbackOutput {
                    apply_id,
                    files_restored: 0,
                    files_removed: 0,
                    non_file_actions: Vec::new(),
                }),
        );
        return Ok(());
    }

    if !yes {
        let confirmed = printer
            .prompt_confirm("Roll back to this apply?")
            .unwrap_or(false);
        if !confirmed {
            printer.emit(
                Doc::new()
                    .status(Role::Info, "Aborted")
                    .with_data(&RollbackOutput {
                        apply_id,
                        files_restored: 0,
                        files_removed: 0,
                        non_file_actions: Vec::new(),
                    }),
            );
            return Ok(());
        }
    }

    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    // A rollback restores a file set, so it reports under the phase name file
    // work carries everywhere else in cfgd.
    let result = {
        let rb_sec =
            printer.section_phase(&cfgd_core::reconciler::PhaseName::Files.section_label());
        // `restore_file_from_backup` warns through a bare `&Printer`; without
        // inheritance its warnings would land at column 0, outside the phase
        // whose files they are about.
        let _inherit = printer.depth_inheritance();
        let r = reconciler.rollback_apply(apply_id, printer)?;
        let processed = r.files_restored + r.files_removed;
        let (role, msg) = if processed == 0 {
            (Role::Info, "No files affected".to_string())
        } else {
            (
                Role::Ok,
                format!("{} processed", cfgd_core::pluralize(processed, "file")),
            )
        };
        rb_sec.status_simple(role, msg);
        if r.files_restored > 0 {
            rb_sec.status_simple(
                Role::Ok,
                format!(
                    "{} restored from backup",
                    cfgd_core::pluralize(r.files_restored, "file")
                ),
            );
        }
        if r.files_removed > 0 {
            rb_sec.status_simple(
                Role::Ok,
                format!(
                    "{} newly created {} removed",
                    r.files_removed,
                    cfgd_core::plural_noun(r.files_removed, "file")
                ),
            );
        }
        r
    };

    if !result.non_file_actions.is_empty() {
        printer.status_simple(
            Role::Warn,
            format!(
                "{} {} manual review",
                cfgd_core::pluralize(result.non_file_actions.len(), "non-file action"),
                cfgd_core::agreeing_verb(result.non_file_actions.len(), "require")
            ),
        );
        let nf_sec = printer.section("Actions");
        for (action_type, resource_id) in &result.non_file_actions {
            // A "script" resource_id is the raw journal-recorded run_str
            // body — condense only for this bullet, never the payload below.
            if action_type == "script" {
                nf_sec.bullet(condense_script_label(resource_id));
            } else {
                nf_sec.bullet(resource_id);
            }
        }
    }

    printer.emit(build_rollback_doc(&RollbackOutput {
        apply_id,
        files_restored: result.files_restored,
        files_removed: result.files_removed,
        non_file_actions: result
            .non_file_actions
            .iter()
            .map(|(_, rid)| rid.clone())
            .collect(),
    }));

    Ok(())
}

/// Sole place the Rollback final Role/subject is decided. Inline emits at
/// every callsite would fork the rule; route through this helper instead.
pub fn build_rollback_doc(output: &RollbackOutput) -> Doc {
    let (role, subject) = if output.files_restored == 0 && output.files_removed == 0 {
        (Role::Info, "No files were changed during rollback")
    } else {
        (Role::Ok, "Rollback complete")
    };
    Doc::new().status(role, subject).with_data(output)
}
