use super::*;

// --- Submodule declarations ---

mod add;
mod create;
mod edit;
mod helpers;
pub mod list;
mod override_cmd;
mod priority;
mod remove;
mod replace;
pub mod show;
mod update;

// --- Re-export handlers so cli::mod can dispatch to them ---

pub use add::cmd_source_add;
pub use create::cmd_source_create;
pub use edit::cmd_source_edit;
pub use list::cmd_source_list;
pub use override_cmd::cmd_source_override;
pub use priority::cmd_source_priority;
pub use remove::cmd_source_remove;
pub use replace::cmd_source_replace;
pub use show::cmd_source_show;
pub use update::{SubscriptionEdits, cmd_source_update};

// Public so an integration test can drive the fetch loop without
// `cmd_source_update`'s process-exiting tail aborting the test binary.
pub use update::run_source_update;

/// The detail half of the ONE failure row a per-source operation settles on,
/// rendered under the `source:<name>` owner section the caller has already
/// opened (`✗ sync failed — <cause>`, `✗ update failed — <cause>`).
///
/// A [`SourceError`] hands back its `cause()` rather than its `Display`,
/// because the owner heading directly above the row already names the source;
/// the full sentence would put that name on the line twice. Anything else
/// collapses as it always did — those errors carry no name to strip.
///
/// [`SourceError`]: cfgd_core::errors::SourceError
pub(in crate::cli) fn source_failure_detail(err: &cfgd_core::errors::CfgdError) -> String {
    match err {
        cfgd_core::errors::CfgdError::Source(source_err) => {
            cfgd_core::output::collapse_to_subject_line(source_err.cause())
        }
        other => cfgd_core::output::collapse_to_subject_line(other),
    }
}

/// The Title Case label a human surface reads for each `subscription` knob,
/// keyed by the YAML key the knob is written under.
///
/// `cfgd source update` rendered the wire key as its row subject
/// (`√ requireSignedCommits — false → true`) on the very command whose job is
/// to set the knob, while `cfgd source show` two screens later called the same
/// fact `Require Signed Commits`. One table, read by both, so the knob cannot
/// have two names.
const SUBSCRIPTION_KNOB_LABELS: &[(&str, &str)] = &[
    ("requireSignedCommits", "Require Signed Commits"),
    ("allowScripts", "Scripts Allowed"),
];

/// The label for one subscription knob's wire key, falling back to the key
/// itself for a knob nothing has named yet — a rendered wire key is a defect,
/// but hiding the row would be a worse one.
pub(in crate::cli) fn subscription_knob_label(key: &str) -> &str {
    SUBSCRIPTION_KNOB_LABELS
        .iter()
        .find(|(k, _)| *k == key)
        .map_or(key, |(_, label)| *label)
}

/// The next step for a per-source failure, for the hint under the ONE failure
/// row `sync` and `update` settle on.
///
/// A refusal is the one screen where the reader is blocked and has to choose
/// between real actions, and it was the only one offering none. Every arm names
/// something the reader can DO; nothing here restates the cause, which the
/// detail beside the row already carries.
pub(in crate::cli) fn source_failure_next_step(
    err: &cfgd_core::errors::CfgdError,
    name: &str,
) -> String {
    use cfgd_core::errors::SourceError;
    match err {
        cfgd_core::errors::CfgdError::Source(SourceError::SignatureVerificationFailed {
            ..
        }) => format!(
            "Sign the HEAD commit, or run `cfgd source update {name} --no-require-signed-commits`"
        ),
        cfgd_core::errors::CfgdError::Source(SourceError::PinRefNotFound { .. }) => {
            format!("Pick an existing ref with `cfgd source update {name} --pin-version <ref>`")
        }
        cfgd_core::errors::CfgdError::Source(SourceError::NotFound { .. }) => {
            "Run `cfgd source list` to see the subscribed sources".to_string()
        }
        cfgd_core::errors::CfgdError::Source(SourceError::InvalidManifest { .. }) => {
            format!("Fix the source's manifest, then run `cfgd source update {name}`")
        }
        // Fetch, git, cache: a transport or a local-cache failure the reader
        // retries once the cause the detail names is gone.
        _ => format!("Retry with `cfgd source update {name}` once the cause above is resolved"),
    }
}

/// What a mutating `source` verb just did, for [`source_success_next_step`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cli) enum SourceMutation {
    /// `source add` recorded a subscription.
    Subscribed,
    /// `source update` fetched, or wrote a subscription knob. `trust_changed`
    /// is the `requireSignedCommits` knob — the one edit whose effect lands on
    /// the NEXT FETCH rather than the next apply.
    Updated { trust_changed: bool },
    /// `source remove` took a subscription out of the composition.
    Removed,
    /// `source replace` re-homed a subscription.
    Replaced,
    /// `source override` set or rejected one of the source's recommendations.
    Overridden,
    /// `source priority` re-ranked a subscription's layer.
    Reprioritized,
}

/// The next step a mutating `source` verb closes on when it SUCCEEDS — the
/// success-side twin of [`source_failure_next_step`], one function so the
/// family cannot drift verb by verb.
///
/// `source update --require-signed-commits` ended on `√ Updated 1 source` and
/// the prompt, on the one command in the take whose effect is on the next
/// fetch, while every other mutating beat said what to type next. A trust edit
/// points at `cfgd sync`, which is where the demand is met; every edit to the
/// COMPOSITION — a subscription added, removed, re-homed, re-ranked or
/// overridden, or a fetch that landed new content — points at the preview and
/// the apply that settle it.
pub(in crate::cli) fn source_success_next_step(mutation: SourceMutation) -> &'static str {
    match mutation {
        SourceMutation::Updated {
            trust_changed: true,
        } => "Run `cfgd sync` to fetch under the new policy",
        SourceMutation::Subscribed
        | SourceMutation::Updated {
            trust_changed: false,
        }
        | SourceMutation::Removed
        | SourceMutation::Replaced
        | SourceMutation::Overridden
        | SourceMutation::Reprioritized => MSG_RUN_APPLY,
    }
}

/// Warning emitted when writing `sources.lock` fails after a source mutation.
/// The lockfile is advisory (it records resolved commit SHAs), so every caller
/// warns and continues rather than failing the command.
pub(in crate::cli) fn sources_lock_update_warning(e: impl std::fmt::Display) -> String {
    format!(
        "Could not update sources.lock: {}",
        cfgd_core::output::collapse_to_subject_line(e)
    )
}

// --- Helpers consumed elsewhere in cli:: ---

pub(in crate::cli) use helpers::{
    build_pending_decisions_table_section, build_permission_input, mutate_config_yaml,
    source_cache_dir,
};

#[cfg(test)]
pub(in crate::cli) use helpers::{
    DEFAULT_NONINTERACTIVE_PRIORITY, add_source_to_config, build_subscription_preview_input,
    count_policy_items, format_conflict_preview_lines, infer_source_name, parse_priority_input,
    remove_source_from_config, resolve_non_interactive_profile,
};

// Glob-import all helpers so siblings can reference them as `super::*`-imported
// names. Load-bearing: covers helpers that are not in the explicit re-export
// lists above (e.g. `add_source_to_config` outside test builds, `with_source_config`).
use helpers::*;
