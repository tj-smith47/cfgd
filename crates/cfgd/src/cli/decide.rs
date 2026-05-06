use super::*;

pub(super) fn cmd_decide(
    printer: &Printer,
    action: DecideAction,
    resource: Option<&str>,
    source: Option<&str>,
    all: bool,
    state_dir: Option<&Path>,
) -> anyhow::Result<()> {
    let resolution = action.resolution();

    let state = open_state_store(state_dir)?;

    if all {
        let count = state.resolve_all_decisions(resolution)?;
        if count == 0 {
            printer.info("No pending decisions");
        } else {
            printer.success(&format!(
                "{} {} item{}",
                resolution.to_uppercase(),
                count,
                if count == 1 { "" } else { "s" }
            ));
            printer.info("Changes will take effect on next reconcile");
        }
        return Ok(());
    }

    if let Some(source_name) = source {
        let count = state.resolve_decisions_for_source(source_name, resolution)?;
        if count == 0 {
            printer.info(&format!(
                "No pending decisions for source '{}'",
                source_name
            ));
        } else {
            printer.success(&format!(
                "{} {} item{} from {}",
                resolution.to_uppercase(),
                count,
                if count == 1 { "" } else { "s" },
                source_name,
            ));
            printer.info("Changes will take effect on next reconcile");
        }
        return Ok(());
    }

    if let Some(resource_path) = resource {
        let resolved = state.resolve_decision(resource_path, resolution)?;
        if resolved {
            printer.success(&format!(
                "{}: {} will {} on next reconcile",
                resolution.to_uppercase(),
                resource_path,
                if resolution == "accepted" {
                    "be applied"
                } else {
                    "not be applied"
                }
            ));
        } else {
            printer.warning(&format!(
                "No pending decision found for '{}'",
                resource_path
            ));
        }
        return Ok(());
    }

    // No resource, source, or --all — show pending decisions
    let decisions = state.pending_decisions()?;
    if decisions.is_empty() {
        printer.info("No pending decisions");
        return Ok(());
    }

    printer.subheader("Pending Decisions");
    display_pending_decisions(printer, &decisions);
    printer.newline();
    printer
        .info("Use `cfgd decide accept <resource>` or `cfgd decide reject <resource>` to resolve");
    printer.info("Use `cfgd decide accept --all` or `cfgd decide accept --source <name>` for bulk operations");

    Ok(())
}
