use super::*;
use anyhow::Context;
use cfgd_core::PathDisplayExt;
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
    /// Source batches no decision row can name (a dotted custom manager) —
    /// withheld from every plan fail-closed, so the listing names them here
    /// instead of showing clean-empty. Same lines the `plan` payload's
    /// `warnings` carries; absent when there are none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// True when the source-decision classification failed and `decisions`
    /// holds only the recorded rows — a degraded listing is otherwise
    /// indistinguishable from a clean empty one to a `-o json` consumer.
    pub classification_degraded: bool,
    /// The machine-stable cause class, present only when degraded — the
    /// reason string beside it is the human detail and carries no stability
    /// promise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classification_degraded_code: Option<super::output_types::ClassificationDegradedCode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classification_degraded_reason: Option<String>,
}

pub(super) fn cmd_decide(
    cli: &Cli,
    printer: &Printer,
    action: Option<DecideAction>,
    resource: Option<&str>,
    source: Option<&str>,
    all: bool,
) -> anyhow::Result<()> {
    // A target without a verb is unanswerable: nothing says which way the
    // named decision(s) should go, and guessing either way resolves rows the
    // operator never asked to resolve. The bare form (no verb, no target) is
    // the read-only listing below.
    let resolution = match action {
        Some(action) => Some(action.resolution()),
        None if all || source.is_some() || resource.is_some() => {
            anyhow::bail!("specify an action (accept or reject) to resolve pending decisions")
        }
        None => None,
    };
    let ctx = RunContext::new(cli, printer);
    let state = ctx.state()?;

    // A resolution is inherently a write, so an item `cfgd plan` classified
    // that nothing has recorded yet becomes a real row HERE, through the same
    // `mint_decisions` machinery apply mints with — the instruction the plan
    // prints (``run `cfgd decide accept/reject` ``) is honoured by the command
    // it names, instead of decide denying the item exists. The mint is
    // narrowed to the target(s) being ANSWERED, and stamps no source hashes:
    // recording the rest of the classification would consume the daemon's one
    // notification for items the operator never touched. Gated exactly as
    // apply's writes are: a foreign config does not write rows into the
    // default store. The bare listing stays a read, like `cfgd plan`.
    let targets = if all {
        Some(reconciler::DecisionTargets::All)
    } else if let Some(name) = source {
        Some(reconciler::DecisionTargets::Source(name))
    } else {
        resource.map(reconciler::DecisionTargets::Resource)
    };
    let writes = match targets {
        Some(t)
            if reconciler::owns_decision_store(
                &cli.config,
                cli.state_dir.is_some(),
                cli.scope(),
            ) =>
        {
            plan_ops::DecisionWrites::Mint(t)
        }
        _ => plan_ops::DecisionWrites::ReadOnly,
    };
    let classification = source_classification(&ctx, state, writes);

    // A verb with no target falls through to the same listing the bare form
    // renders: there is nothing to resolve, and showing what could be is more
    // useful than refusing.
    if let Some(resolution) = resolution {
        if all {
            let count = state.resolve_all_decisions(resolution)?;
            if count == 0
                && let Err(e) = classification
            {
                return Err(e.context("no recorded decisions, and the unrecorded items could not be classified to answer them"));
            }
            printer.emit(build_decide_bulk_doc(resolution, count, None));
            return Ok(());
        }

        if let Some(source_name) = source {
            let count = state.resolve_decisions_for_source(source_name, resolution)?;
            if count == 0
                && let Err(e) = classification
            {
                return Err(e.context(format!(
                    "no recorded decisions for source '{source_name}', and its unrecorded items could not be classified to answer them"
                )));
            }
            printer.emit(build_decide_bulk_doc(resolution, count, Some(source_name)));
            return Ok(());
        }

        if let Some(resource_path) = resource {
            let resolved = state.resolve_decision(resource_path, resolution)?;
            if !resolved && let Err(e) = classification {
                return Err(e.context(format!(
                    "no recorded decision matches '{resource_path}', and the unrecorded items could not be classified to answer it"
                )));
            }
            printer.emit(build_decide_single_doc(resolution, resource_path, resolved));
            return Ok(());
        }
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
    // without a row being minted for the offer itself. A listing is a
    // dashboard, not an answer, so a broken classification degrades it — the
    // recorded rows still list, with a warning that the unrecorded ones could
    // not be read — where a resolving invocation above refuses outright.
    let mut classification_degraded: Option<(
        super::output_types::ClassificationDegradedCode,
        String,
    )> = None;
    let mut warnings: Vec<String> = Vec::new();
    let mut composed: Option<cfgd_core::config::ResolvedProfile> = None;
    match classification {
        Ok((withheld, _, resolved)) => {
            warnings = withheld.undecidable.iter().map(|b| b.warning()).collect();
            decisions.extend(withheld.pending.into_iter().filter(|d| d.id == 0));
            composed = resolved;
        }
        Err(e) => {
            let code = super::output_types::ClassificationDegradedCode::from_error(&e);
            let reason = cfgd_core::output::collapse_to_subject_line(format!("{e:#}"));
            printer.status_simple(
                Role::Warn,
                format!("Unrecorded source items not listed: {reason}"),
            );
            classification_degraded = Some((code, reason));
        }
    }
    let contents = match &composed {
        Some(resolved) => {
            super::DecisionContents::for_decisions(resolved, &decisions, &super::config_dir(cli))
        }
        None => Default::default(),
    };
    printer.emit(build_decide_list_doc(
        &decisions,
        &warnings,
        classification_degraded,
        &contents,
    ));
    Ok(())
}

/// The shared source-decision classification, built the way `cfgd plan` builds
/// it but composing cache-only — decide stays offline — and in `Report` mode,
/// with the other classification reads: decide's own write is a row in the
/// decision store, never a change to the machine, and `Enforce` would disable
/// answering exactly when a source violates a constraint.
///
/// The error carries WHICH input was unreadable. A resolving decide refuses on
/// it rather than answering "no pending decision found" — indistinguishable
/// from an empty store — while the bare listing degrades to recorded rows
/// with a warning. Nothing is minted from a broken picture either way, and a
/// row that exists withholds (and resolves) regardless of what this returns.
///
/// Classification only applies where sources exist to classify: a config file
/// that does not exist cannot have subscribed this machine to anything (its
/// runs never load, so nothing was ever offered from it), and a readable
/// config with zero sources has no source items either way — both answer
/// "nothing unrecorded" instead of running composition, so a local manifest
/// typo on a sourceless machine cannot disable answering the store's rows.
///
/// The resolved profile travels back out with the verdict because the listing
/// renders each pending row's CONTENT, and this is the one composition the
/// command performs — deriving it again at the render would be a second
/// config parse per invocation. `None` is a run with nothing to classify.
type Classification = (
    reconciler::WithheldDecisions,
    reconciler::SourcePolicyReview,
    Option<cfgd_core::config::ResolvedProfile>,
);

fn source_classification(
    ctx: &RunContext<'_>,
    state: &cfgd_core::state::StateStore,
    writes: plan_ops::DecisionWrites<'_>,
) -> anyhow::Result<Classification> {
    let cli = ctx.cli();
    if !cli.config.exists() {
        return Ok(Default::default());
    }
    let (cfg, _profile_name, local_resolved) = ctx
        .config_and_profile()
        .with_context(|| format!("config {} is unreadable", cli.config.posix()))?;
    if cfg.spec.sources.is_empty() {
        return Ok(Default::default());
    }
    let desired = resolve_desired_state(
        ctx,
        cfg,
        local_resolved,
        &[],
        false,
        ctx.printer(),
        false,
        composition::ConstraintMode::Report,
    )
    .context("source composition failed")?;
    // Decide enumerates no package state (it stays offline), so the
    // classification auto-accepts nothing here — installed-but-undecided items
    // keep listing until a run that enumerates (plan/apply/tick) releases them.
    let (withheld, review) = plan_ops::withheld_for_run(
        ctx,
        state,
        cfg,
        &desired.resolved,
        true,
        writes,
        &reconciler::ActualPackages::default(),
    )
    .context("source classification failed")?;
    Ok((withheld, review, Some(desired.resolved)))
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

/// Pure builder: pending-decisions listing Doc (bare `cfgd decide`). A
/// `Some` (code, reason) pair marks the payload as degraded — the human
/// warning for it is the caller's, printed where the failure happened.
/// `warnings` names the undecidable source batches no row can carry, so an
/// otherwise-empty listing never reads as "nothing withheld" while a dotted
/// custom manager's packages are.
pub fn build_decide_list_doc(
    decisions: &[PendingDecision],
    warnings: &[String],
    classification_degraded: Option<(super::output_types::ClassificationDegradedCode, String)>,
    contents: &super::DecisionContents,
) -> Doc {
    let payload = DecideListOutput {
        decisions: decisions.to_vec(),
        warnings: warnings.to_vec(),
        classification_degraded: classification_degraded.is_some(),
        classification_degraded_code: classification_degraded.as_ref().map(|(c, _)| *c),
        classification_degraded_reason: classification_degraded.map(|(_, r)| r),
    };
    let warn_lines =
        |doc: Doc| -> Doc { warnings.iter().fold(doc, |d, w| d.status(Role::Warn, w)) };
    if decisions.is_empty() {
        return warn_lines(Doc::new().status(Role::Info, "No pending decisions"))
            .with_data(payload);
    }

    warn_lines(Doc::new().section("Pending Decisions", |s| {
        build_pending_decisions_table_section(s, decisions, contents)
    }))
    .hint("Use `cfgd decide accept <resource>` or `cfgd decide reject <resource>` to resolve")
    .hint(
        "Use `cfgd decide accept --all` or `cfgd decide accept --source <name>` for bulk operations",
    )
    .with_data(payload)
}
