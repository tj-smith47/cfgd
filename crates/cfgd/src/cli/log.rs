use super::*;

use cfgd_core::output_v2::{Doc, Printer as PrinterV2, Role, renderer::Table};

pub fn cmd_log(
    v2_printer: &PrinterV2,
    count: u32,
    show_output: Option<i64>,
    state_dir: Option<&Path>,
) -> anyhow::Result<()> {
    let state = open_state_store(state_dir)?;

    if let Some(apply_id) = show_output {
        return cmd_log_show_output(v2_printer, &state, apply_id);
    }

    let history = state.history(count)?;
    v2_printer.emit(build_log_doc(&LogOutput { entries: history }));
    Ok(())
}

fn cmd_log_show_output(
    v2_printer: &PrinterV2,
    state: &cfgd_core::state::StateStore,
    apply_id: i64,
) -> anyhow::Result<()> {
    if state.get_apply(apply_id)?.is_none() {
        anyhow::bail!("no apply found with ID {}", apply_id);
    }
    let entries = state.journal_entries(apply_id)?;

    if entries.is_empty() {
        v2_printer.emit(
            Doc::new()
                .heading(format!("Apply #{} — Script Output", apply_id))
                .status(
                    Role::Info,
                    format!("No journal entries for apply #{}", apply_id),
                )
                .with_data(&LogShowOutputOutput {
                    apply_id,
                    entries: Vec::new(),
                }),
        );
        return Ok(());
    }

    v2_printer.heading(format!("Apply #{} — Script Output", apply_id));

    let mut payload_entries = Vec::new();
    let mut found_output = false;
    {
        let entries_sec = v2_printer.section_or_collapse("Entries");
        for entry in &entries {
            if let Some(ref output) = entry.script_output {
                found_output = true;
                let entry_sec = entries_sec.section(format!(
                    "[{}] {} ({})",
                    entry.phase, entry.resource_id, entry.action_type,
                ));
                for line in output.lines() {
                    entry_sec.status_simple(Role::Info, line);
                }
                payload_entries.push(LogShowEntryOutput {
                    phase: entry.phase.clone(),
                    resource_id: entry.resource_id.clone(),
                    action_type: entry.action_type.clone(),
                    output: output.clone(),
                });
            }
        }
    }

    if !found_output {
        v2_printer.status_simple(Role::Info, "No script output captured for this apply");
    }

    v2_printer.emit(Doc::new().with_data(&LogShowOutputOutput {
        apply_id,
        entries: payload_entries,
    }));
    Ok(())
}

/// Build the buffered `Doc` for the apply-history view. Pure function so
/// snapshot tests can drive the JSON path without standing up a seeded state
/// DB for every assertion.
pub fn build_log_doc(output: &LogOutput) -> Doc {
    let mut doc = Doc::new().heading("Apply History");
    if output.entries.is_empty() {
        doc = doc.status(Role::Info, "No applies recorded yet");
    } else {
        doc = doc.table(Table {
            headers: vec![
                "ID".into(),
                "Time".into(),
                "Profile".into(),
                "Status".into(),
                "Summary".into(),
            ],
            rows: output
                .entries
                .iter()
                .map(|record| {
                    vec![
                        record.id.to_string(),
                        record.timestamp.clone(),
                        record.profile.clone(),
                        match record.status {
                            cfgd_core::state::ApplyStatus::Success => "success".into(),
                            cfgd_core::state::ApplyStatus::Partial => "partial".into(),
                            cfgd_core::state::ApplyStatus::Failed => "failed".into(),
                            cfgd_core::state::ApplyStatus::InProgress => "in_progress".into(),
                        },
                        record.summary.clone().unwrap_or_else(|| "-".into()),
                    ]
                })
                .collect(),
        });
    }
    doc.with_data(output)
}
