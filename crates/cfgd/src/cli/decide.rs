use super::*;
use cfgd_core::output::{Doc, Printer, Role};
use cfgd_core::state::PendingDecision;

/// Bulk-resolution payload (`accept --all` or `accept --source <name>`).
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DecideBulkOutput {
    pub resolution: String,
    pub count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Single-resource resolution payload (`accept <resource>` / `reject <resource>`).
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DecideSingleOutput {
    pub resolution: String,
    pub resource: String,
    pub resolved: bool,
}

/// Listing payload (bare `cfgd decide` with no args).
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DecideListOutput {
    pub decisions: Vec<PendingDecision>,
}

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
        printer.emit(build_decide_bulk_doc(resolution, count, None));
        return Ok(());
    }

    if let Some(source_name) = source {
        let count = state.resolve_decisions_for_source(source_name, resolution)?;
        printer.emit(build_decide_bulk_doc(resolution, count, Some(source_name)));
        return Ok(());
    }

    if let Some(resource_path) = resource {
        let resolved = state.resolve_decision(resource_path, resolution)?;
        printer.emit(build_decide_single_doc(resolution, resource_path, resolved));
        return Ok(());
    }

    let decisions = state.pending_decisions()?;
    printer.emit(build_decide_list_doc(&decisions));
    Ok(())
}

/// Pure builder: bulk-resolution Doc (`accept --all` / `accept --source`).
pub fn build_decide_bulk_doc(resolution: &str, count: usize, source: Option<&str>) -> Doc {
    let mut doc = Doc::new();
    if count == 0 {
        let msg = match source {
            None => "No pending decisions".to_string(),
            Some(name) => format!("No pending decisions for source '{name}'"),
        };
        doc = doc.status(Role::Info, msg);
    } else {
        let plural = if count == 1 { "" } else { "s" };
        let verb = resolution.to_uppercase();
        let msg = match source {
            None => format!("{verb} {count} item{plural}"),
            Some(name) => format!("{verb} {count} item{plural} from {name}"),
        };
        doc = doc
            .status(Role::Ok, msg)
            .hint("Changes will take effect on next reconcile");
    }
    doc.with_data(DecideBulkOutput {
        resolution: resolution.to_string(),
        count,
        source: source.map(str::to_string),
    })
}

/// Pure builder: single-resource resolution Doc.
pub fn build_decide_single_doc(resolution: &str, resource_path: &str, resolved: bool) -> Doc {
    let mut doc = Doc::new();
    if resolved {
        let verb = if resolution == "accepted" {
            "be applied"
        } else {
            "not be applied"
        };
        doc = doc.status(
            Role::Ok,
            format!(
                "{}: {} will {} on next reconcile",
                resolution.to_uppercase(),
                resource_path,
                verb
            ),
        );
    } else {
        doc = doc.status(
            Role::Warn,
            format!("No pending decision found for '{resource_path}'"),
        );
    }
    doc.with_data(DecideSingleOutput {
        resolution: resolution.to_string(),
        resource: resource_path.to_string(),
        resolved,
    })
}

/// Pure builder: pending-decisions listing Doc (bare `cfgd decide`).
pub fn build_decide_list_doc(decisions: &[PendingDecision]) -> Doc {
    if decisions.is_empty() {
        return Doc::new()
            .status(Role::Info, "No pending decisions")
            .with_data(DecideListOutput { decisions: vec![] });
    }

    Doc::new()
        .section("Pending Decisions", |s| {
            build_pending_decisions_table_section(s, decisions)
        })
        .hint("Use `cfgd decide accept <resource>` or `cfgd decide reject <resource>` to resolve")
        .hint(
            "Use `cfgd decide accept --all` or `cfgd decide accept --source <name>` for bulk operations",
        )
        .with_data(DecideListOutput {
            decisions: decisions.to_vec(),
        })
}
