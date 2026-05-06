use super::*;

pub(super) fn cmd_log(
    printer: &Printer,
    count: u32,
    show_output: Option<i64>,
    state_dir: Option<&Path>,
) -> anyhow::Result<()> {
    let state = open_state_store(state_dir)?;

    if let Some(apply_id) = show_output {
        return cmd_log_show_output(printer, &state, apply_id);
    }

    let history = state.history(count)?;

    if printer.is_structured() {
        printer.write_structured(&LogOutput { entries: history });
        return Ok(());
    }

    printer.header("Apply History");

    if history.is_empty() {
        printer.newline();
        printer.info("No applies recorded yet");
        return Ok(());
    }

    printer.newline();
    printer.table(
        &["ID", "Time", "Profile", "Status", "Summary"],
        &history
            .iter()
            .map(|record| {
                vec![
                    record.id.to_string(),
                    record.timestamp.clone(),
                    record.profile.clone(),
                    match record.status {
                        cfgd_core::state::ApplyStatus::Success => "success".to_string(),
                        cfgd_core::state::ApplyStatus::Partial => "partial".to_string(),
                        cfgd_core::state::ApplyStatus::Failed => "failed".to_string(),
                        cfgd_core::state::ApplyStatus::InProgress => "in_progress".to_string(),
                    },
                    record.summary.clone().unwrap_or_else(|| "-".to_string()),
                ]
            })
            .collect::<Vec<_>>(),
    );

    Ok(())
}

pub(super) fn cmd_log_show_output(
    printer: &Printer,
    state: &cfgd_core::state::StateStore,
    apply_id: i64,
) -> anyhow::Result<()> {
    // Verify the apply ID exists before showing output
    if state.get_apply(apply_id)?.is_none() {
        anyhow::bail!("no apply found with ID {}", apply_id);
    }

    let entries = state.journal_entries(apply_id)?;

    if entries.is_empty() {
        printer.info(&format!("No journal entries for apply #{}", apply_id));
        return Ok(());
    }

    printer.header(&format!("Apply #{} — Script Output", apply_id));

    let mut found_output = false;
    for entry in &entries {
        if let Some(ref output) = entry.script_output {
            found_output = true;
            printer.newline();
            printer.subheader(&format!(
                "[{}] {} ({})",
                entry.phase, entry.resource_id, entry.action_type
            ));
            for line in output.lines() {
                printer.info(line);
            }
        }
    }

    if !found_output {
        printer.newline();
        printer.info("No script output captured for this apply");
    }

    Ok(())
}
