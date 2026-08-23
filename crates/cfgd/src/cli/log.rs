use super::*;

use cfgd_core::output::{Doc, Printer, Role, TitleLabel, condense_script_label, renderer::Table};

pub fn cmd_log(
    printer: &Printer,
    count: u32,
    show_output: Option<i64>,
    state_dir: Option<&Path>,
    scope: cfgd_core::Scope,
) -> anyhow::Result<()> {
    let state = open_state_store(state_dir, scope)?;

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
        // Typed StateError::ApplyNotFound so the exit-code downcast resolves to
        // ExitCode::NotFound (6), uniform with `cfgd rollback <id>` — the sibling
        // apply-ID lookup — and every other named-resource miss (backup name,
        // snapshot, module, profile), and so `-o json` gets the stable
        // `{"error":"not_found",...}` payload instead of nothing.
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
    let entries = state.journal_entries(apply_id)?;

    if entries.is_empty() {
        printer.emit(
            Doc::new()
                .heading_title(format!("Apply #{apply_id}"), "Script Output")
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

    printer.heading_title(&TitleLabel::new(
        format!("Apply #{apply_id}"),
        "Script Output",
    ));

    let mut payload_entries = Vec::new();
    let mut found_output = false;
    {
        let entries_sec = printer.section_or_collapse("Entries");
        for entry in &entries {
            if let Some(ref output) = entry.script_output {
                found_output = true;
                // `entry.resource_id` for a "script" journal row is the raw
                // run_str body (see `format_action_description`'s
                // `Action::Script` arm) — condense only in this section
                // header, never the persisted/payload copy below.
                let display_id = if entry.action_type == "script" {
                    condense_script_label(&entry.resource_id)
                } else {
                    entry.resource_id.clone()
                };
                let entry_sec = entries_sec.section(format!(
                    "[{}] {} ({})",
                    entry.phase, display_id, entry.action_type,
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
                    record.status.human_str().to_string(),
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

    /// The `cfgd log` human table Status column renders the TitleCase display
    /// vocabulary (`human_str`), never the snake_case persistence form
    /// (`in_progress`) and never the camelCase WIRE token (`inProgress`) that
    /// the `-o json` payload beside it still carries.
    #[test]
    fn log_table_status_column_is_the_titlecase_display_word() {
        let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
        printer.emit(build_log_doc(&in_progress_log()));
        drop(printer);

        let out = cfgd_core::test_helpers::captured_text(&buf);
        assert!(
            out.contains("InProgress"),
            "log Status column should render human_str word, got: {out}"
        );
        assert!(
            !out.contains("in_progress"),
            "log Status column leaked the snake_case persistence form, got: {out}"
        );
        assert!(
            !out.contains("inProgress"),
            "log Status column leaked the camelCase wire token, got: {out}"
        );
    }

    /// The `cfgd log -o json` surface must emit the unified camelCase token at
    /// `entries[].status` (the derived `Serialize` path).
    #[test]
    fn log_json_status_is_camelcase_token() {
        let (printer, buf) = Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);
        printer.emit(build_log_doc(&in_progress_log()));
        drop(printer);

        let out = cfgd_core::test_helpers::captured_text(&buf);
        let json: serde_json::Value = serde_json::from_str(&out).expect("valid json");
        assert_eq!(
            json["entries"][0]["status"],
            serde_json::json!("inProgress")
        );
    }

    // `cmd_log_show_output`'s per-entry section header embeds
    // `entry.resource_id` raw — for a "script" journal row that is the raw
    // run_str body, which for a multi-line inline script trips
    // `Renderer::write_line`'s no-embedded-newline assert.
    #[test]
    fn log_show_output_condenses_multiline_script_resource_id_in_section_header() {
        let state_dir = tempfile::tempdir().unwrap();
        let state = cfgd_core::state::StateStore::open(&state_dir.path().join("state.db")).unwrap();
        let apply_id = state
            .record_apply("test", "hash1", ApplyStatus::Success, None)
            .unwrap();
        let raw_body = "echo line-one\necho line-two";
        let jid = state
            .journal_begin(apply_id, 0, "PostScripts", "script", raw_body, None)
            .unwrap();
        state
            .journal_complete(jid, 0, None, Some("captured output"))
            .unwrap();

        let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
        cmd_log_show_output(&printer, &state, apply_id).unwrap();
        drop(printer);

        let out = cfgd_core::test_helpers::captured_text(&buf);
        assert!(
            !out.contains("line-two"),
            "section header must condense away subsequent lines, got: {out}"
        );
        assert!(
            out.contains("echo line-one"),
            "condensed header should reference the first line, got: {out}"
        );
    }

    /// `cfgd log --show-output <id>` naming an apply ID that isn't in the log
    /// is the same "you asked for a thing that is not there" condition as
    /// `cfgd rollback <id>` — its sibling apply-ID lookup — so it must carry
    /// the typed `StateError::ApplyNotFound` and resolve to exit `6`, not a
    /// bare `anyhow!` and the generic exit code.
    #[test]
    fn log_show_output_unknown_apply_id_is_a_typed_not_found_error() {
        let state_dir = tempfile::tempdir().unwrap();
        let state = cfgd_core::state::StateStore::open(&state_dir.path().join("state.db")).unwrap();
        let (printer, _buf) = Printer::for_test_at(Verbosity::Normal);

        let err = cmd_log_show_output(&printer, &state, 999).unwrap_err();
        let cfgd_err = err
            .downcast_ref::<cfgd_core::errors::CfgdError>()
            .expect("an unknown apply ID must carry the typed StateError::ApplyNotFound");
        assert!(
            matches!(
                cfgd_err,
                cfgd_core::errors::CfgdError::State(
                    cfgd_core::errors::StateError::ApplyNotFound { .. }
                )
            ),
            "expected StateError::ApplyNotFound, got: {cfgd_err}"
        );
        assert_eq!(
            cfgd_core::exit::exit_code_for_error(cfgd_err),
            cfgd_core::exit::ExitCode::NotFound
        );
    }
}
