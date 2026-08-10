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
    cli: &Cli,
    printer: &Printer,
    action: DecideAction,
    resource: Option<&str>,
    source: Option<&str>,
    all: bool,
) -> anyhow::Result<()> {
    let resolution = action.resolution();
    let state = open_state_store(cli.state_dir.as_deref())?;

    // A resolution is inherently a write, so an item `cfgd plan` classified
    // that nothing has recorded yet becomes a real row HERE, through the same
    // `mint_decisions` machinery apply mints with — the instruction the plan
    // prints (``run `cfgd decide accept/reject` ``) is honoured by the command
    // it names, instead of decide denying the item exists. Gated exactly as
    // apply's writes are: a foreign config does not write rows into the
    // default store. The bare listing stays a read, like `cfgd plan`.
    let resolving = all || source.is_some() || resource.is_some();
    let writes =
        if resolving && reconciler::owns_decision_store(&cli.config, cli.state_dir.is_some()) {
            plan_ops::DecisionWrites::Mint
        } else {
            plan_ops::DecisionWrites::ReadOnly
        };
    let classification = source_classification(cli, printer, &state, writes);

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

    // Only rows this machine can still act on. A config that will not parse
    // says nothing about which sources are subscribed, so the listing shows
    // everything rather than hiding a row on a guess — listing one is harmless,
    // hiding the one the operator came to answer is not.
    let subscriptions = match config::load_config(&cli.config) {
        Ok(cfg) => reconciler::Subscriptions::known(cfg.spec.sources.iter().map(|s| &s.name)),
        Err(e) => {
            tracing::debug!("config load failed, listing every decision: {}", e);
            reconciler::Subscriptions::Unverified
        }
    };
    let mut decisions = subscriptions.answerable(state.pending_decisions()?);
    // Classified-but-unrecorded items (`id` 0) are offered for an answer
    // without a row being minted for the offer itself.
    if let Some((withheld, _)) = classification {
        decisions.extend(withheld.pending.into_iter().filter(|d| d.id == 0));
    }
    printer.emit(build_decide_list_doc(&decisions));
    Ok(())
}

/// The shared source-decision classification, built the way `cfgd plan` builds
/// it but composing cache-only — decide stays offline. `None` when the config
/// or composition cannot be read: decide must still answer the rows that
/// already exist, and minting from a broken picture is exactly the guess the
/// fail-closed rule forbids (nothing is released either way — a row that
/// exists withholds regardless of what this returns).
fn source_classification(
    cli: &Cli,
    printer: &Printer,
    state: &cfgd_core::state::StateStore,
    writes: plan_ops::DecisionWrites,
) -> Option<(
    reconciler::WithheldDecisions,
    reconciler::SourcePolicyReview,
)> {
    let (cfg, _profile_name, local_resolved) = match load_config_and_profile(cli) {
        Ok(loaded) => loaded,
        Err(e) => {
            tracing::debug!("config load failed, skipping source classification: {e}");
            return None;
        }
    };
    let desired = match resolve_desired_state(
        cli,
        &cfg,
        &local_resolved,
        None,
        printer,
        false,
        composition::ConstraintMode::Enforce,
    ) {
        Ok(d) => d,
        Err(e) => {
            tracing::debug!("composition failed, skipping source classification: {e}");
            return None;
        }
    };
    match plan_ops::withheld_for_run(
        state,
        &cfg,
        &desired.resolved,
        &config_dir(cli),
        true,
        writes,
    ) {
        Ok(classified) => Some(classified),
        Err(e) => {
            tracing::debug!("source classification unreadable: {e}");
            None
        }
    }
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
