use super::*;

use cfgd_core::output::{Doc, Printer, Role};

pub fn cmd_rollback(
    v2_printer: &Printer,
    apply_id: i64,
    yes: bool,
    state_dir: Option<&Path>,
) -> anyhow::Result<()> {
    let state = open_state_store(state_dir)?;

    if state.get_apply(apply_id)?.is_none() {
        anyhow::bail!("no apply found with ID {}", apply_id);
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
        .filter(|e| {
            !(e.phase == "files" || e.action_type == "file" || e.resource_id.starts_with("file:"))
        })
        .map(|e| e.resource_id.clone())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    let non_file_count = non_file_actions.len();

    v2_printer.heading("Rollback");
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
    v2_printer.kv_block(kv_pairs);

    if file_count == 0 && non_file_count == 0 {
        v2_printer.emit(
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
        let confirmed = v2_printer
            .prompt_confirm("Roll back to this apply?")
            .unwrap_or(false);
        if !confirmed {
            v2_printer.emit(
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
    let result = {
        let rb_sec = v2_printer.section("Restoring");
        let r = reconciler.rollback_apply(apply_id, v2_printer)?;
        let processed = r.files_restored + r.files_removed;
        let (role, msg) = if processed == 0 {
            (Role::Info, "No files affected".to_string())
        } else {
            (Role::Ok, format!("{} file(s) processed", processed))
        };
        rb_sec.status_simple(role, msg);
        r
    };

    if result.files_restored > 0 {
        v2_printer.status_simple(
            Role::Ok,
            format!("{} file(s) restored from backup", result.files_restored),
        );
    }
    if result.files_removed > 0 {
        v2_printer.status_simple(
            Role::Ok,
            format!("{} newly created file(s) removed", result.files_removed),
        );
    }

    if !result.non_file_actions.is_empty() {
        v2_printer.status_simple(
            Role::Warn,
            format!(
                "{} non-file action(s) require manual review",
                result.non_file_actions.len()
            ),
        );
        let nf_sec = v2_printer.section("Actions");
        for action in &result.non_file_actions {
            nf_sec.bullet(action);
        }
    }

    v2_printer.emit(build_rollback_doc(&RollbackOutput {
        apply_id,
        files_restored: result.files_restored,
        files_removed: result.files_removed,
        non_file_actions: result.non_file_actions.clone(),
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
