use super::*;

// --- Submodule declarations ---

mod add;
mod create;
mod edit;
mod helpers;
mod list;
mod override_cmd;
mod priority;
mod remove;
mod replace;
pub mod show;
mod update;

// --- Re-export pub(crate) handlers so cli::mod can dispatch to them ---

pub(super) use add::cmd_source_add;
pub(super) use create::cmd_source_create;
pub(super) use edit::cmd_source_edit;
pub(super) use list::cmd_source_list;
pub(super) use override_cmd::cmd_source_override;
pub(super) use priority::cmd_source_priority;
pub(super) use remove::cmd_source_remove;
pub(super) use replace::cmd_source_replace;
pub(super) use show::cmd_source_show;
pub(super) use update::cmd_source_update;

// --- Helpers consumed elsewhere in cli:: ---

#[cfg(test)]
pub(in crate::cli) use helpers::display_pending_decisions;
pub(in crate::cli) use helpers::{
    build_pending_decisions_table_section, build_permission_input, mutate_config_yaml,
    source_cache_dir,
};

#[cfg(test)]
pub(in crate::cli) use helpers::{
    DEFAULT_NONINTERACTIVE_PRIORITY, add_source_to_config, build_subscription_preview_input,
    count_policy_items, display_policy_items, display_source_manifest,
    format_conflict_preview_lines, infer_source_name, parse_priority_input,
    remove_source_from_config, resolve_non_interactive_profile,
};

// Glob-import all helpers so siblings can reference them as `super::*`-imported
// names. Load-bearing: covers helpers that are not in the explicit re-export
// lists above (e.g. `add_source_to_config` outside test builds, `with_source_config`).
use helpers::*;
