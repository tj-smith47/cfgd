use super::*;

use cfgd_core::output::{Doc, Printer, Role, renderer::Table};

pub fn cmd_log(
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
    printer.emit(build_log_doc(&LogOutput { entries: history }));
    Ok(())
}

fn cmd_log_show_output(
    printer: &Printer,
    state: &cfgd_core::state::StateStore,
    apply_id: i64,
) -> anyhow::Result<()> {
    if state.get_apply(apply_id)?.is_none() {
        anyhow::bail!("no apply found with ID {}", apply_id);
    }
    let entries = state.journal_entries(apply_id)?;

    if entries.is_empty() {
        printer.emit(
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

    printer.heading(format!("Apply #{} — Script Output", apply_id));

    let mut payload_entries = Vec::new();
    let mut found_output = false;
    {
        let entries_sec = printer.section_or_collapse("Entries");
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
        printer.status_simple(Role::Info, "No script output captured for this apply");
    }

    printer.emit(Doc::new().with_data(&LogShowOutputOutput {
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
        let rows: Vec<Vec<String>> = output
            .entries
            .iter()
            .map(|record| {
                vec![
                    record.id.to_string(),
                    record.timestamp.clone(),
                    record.profile.clone(),
                    record.status.display_str().to_string(),
                    record.summary.clone().unwrap_or_else(|| "-".into()),
                ]
            })
            .collect();
        let mut t = Table::new(["ID", "Time", "Profile", "Status", "Summary"]);
        for row in rows {
            t = t.row(row);
        }
        doc = doc.table(t);
    }
    doc.with_data(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cfgd_core::output::Verbosity;
    use cfgd_core::state::{ApplyRecord, ApplyStatus};

    fn in_progress_log() -> LogOutput {
        LogOutput {
            entries: vec![ApplyRecord {
                id: 1,
                timestamp: "2026-01-02T03:04:05Z".to_string(),
                profile: "default".to_string(),
                plan_hash: "deadbeef".to_string(),
                status: ApplyStatus::InProgress,
                summary: Some("running".to_string()),
            }],
        }
    }

    /// The `cfgd log` human table Status column must render the unified
    /// camelCase token via `display_str`, never the snake_case persistence
    /// form (`in_progress`) or the bare PascalCase variant (`InProgress`).
    #[test]
    fn log_table_status_column_is_camelcase_token() {
        let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
        printer.emit(build_log_doc(&in_progress_log()));
        drop(printer);

        let out = buf.lock().unwrap();
        assert!(
            out.contains("inProgress"),
            "log Status column should render display_str token, got: {out}"
        );
        assert!(
            !out.contains("in_progress"),
            "log Status column leaked the snake_case persistence form, got: {out}"
        );
        assert!(
            !out.contains("InProgress"),
            "log Status column leaked the bare PascalCase variant, got: {out}"
        );
    }

    /// The `cfgd log -o json` surface must emit the unified camelCase token at
    /// `entries[].status` (the derived `Serialize` path).
    #[test]
    fn log_json_status_is_camelcase_token() {
        let (printer, buf) = Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);
        printer.emit(build_log_doc(&in_progress_log()));
        drop(printer);

        let out = buf.lock().unwrap();
        let json: serde_json::Value = serde_json::from_str(&out).expect("valid json");
        assert_eq!(
            json["entries"][0]["status"],
            serde_json::json!("inProgress")
        );
    }
}
