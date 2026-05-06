use super::*;

pub(super) fn cmd_rollback(
    printer: &Printer,
    apply_id: i64,
    yes: bool,
    state_dir: Option<&Path>,
) -> anyhow::Result<()> {
    let state = open_state_store(state_dir)?;

    // Check if the target apply exists
    if state.get_apply(apply_id)?.is_none() {
        anyhow::bail!("no apply found with ID {}", apply_id);
    }

    // Preview: count file backups available for rollback (target apply's own
    // post-apply snapshots + subsequent apply backups) and non-file actions.
    let target_backups = state.get_apply_backups(apply_id)?;
    let after_backups = state.file_backups_after_apply(apply_id)?;
    let after_entries = state.journal_entries_after_apply(apply_id)?;

    // Unique file paths across both sources
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

    printer.header("Rollback");
    printer.key_value("Target apply ID", &apply_id.to_string());
    printer.key_value("File backups to restore", &file_count.to_string());
    if non_file_count > 0 {
        printer.key_value(
            "Non-file actions (manual review)",
            &non_file_count.to_string(),
        );
    }

    if file_count == 0 && non_file_count == 0 {
        printer.info("No subsequent changes to roll back — system is already at this apply");
        return Ok(());
    }

    // Confirm
    if !yes {
        printer.newline();
        let confirmed = printer
            .prompt_confirm("Roll back to this apply?")
            .unwrap_or(false);
        if !confirmed {
            printer.info("Aborted");
            return Ok(());
        }
    }

    printer.newline();

    // Construct a minimal Reconciler — rollback only needs state, but Reconciler
    // requires a ProviderRegistry reference.
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let result = reconciler.rollback_apply(apply_id, printer)?;

    if printer.is_structured() {
        printer.write_structured(&RollbackOutput {
            apply_id,
            files_restored: result.files_restored,
            files_removed: result.files_removed,
            non_file_actions: result.non_file_actions.clone(),
        });
        return Ok(());
    }

    printer.newline();
    if result.files_restored > 0 {
        printer.success(&format!(
            "{} file(s) restored from backup",
            result.files_restored
        ));
    }
    if result.files_removed > 0 {
        printer.success(&format!(
            "{} newly created file(s) removed",
            result.files_removed
        ));
    }

    if !result.non_file_actions.is_empty() {
        printer.newline();
        printer.warning(&format!(
            "{} non-file action(s) require manual review:",
            result.non_file_actions.len()
        ));
        for action in &result.non_file_actions {
            printer.info(&format!("  {}", action));
        }
    }

    if result.files_restored == 0 && result.files_removed == 0 {
        printer.info("No files were changed during rollback");
    } else {
        printer.newline();
        printer.success("Rollback complete");
    }

    Ok(())
}
