//! Dogfood test: cfgd's CLI tree must produce a valid brontes tool list under
//! the configuration the binary actually ships.
//!
//! Exercises brontes' library API against the full cfgd clap surface so that
//! a regression in either crate (cfgd Cli reshape, brontes walker behaviour)
//! is caught here before it reaches downstream consumers. `generate_tools`
//! rejects a config path that names no walked command, so the annotation and
//! group tables in `cfgd::mcp::brontes` are pinned against the clap tree too.

use cfgd::{Cli, mcp::brontes as mcp};
use clap::CommandFactory;

/// Every tool the config can serve, with the shipped default trim lifted.
///
/// The classification and hiding rules below are properties of the whole
/// surface, not of whichever slice happens to be pinned — checking them
/// against the trimmed default would stop covering a command the moment it
/// fell outside the pinned group.
fn all_tools() -> Vec<brontes::Tool> {
    brontes::generate_tools(&Cli::command(), &mcp::config().expose_all())
        .expect("cfgd CLI must produce a valid brontes tool list")
}

fn tool_names() -> Vec<String> {
    all_tools().iter().map(|t| t.name.to_string()).collect()
}

/// Tool names a server started with no selection flags actually serves.
fn shipped_tool_names() -> Vec<String> {
    brontes::generate_tools(&Cli::command(), &mcp::config())
        .expect("the shipped config must produce a tool list")
        .iter()
        .map(|t| t.name.to_string())
        .collect()
}

#[test]
fn cfgd_cli_produces_valid_brontes_tool_list() {
    let names = tool_names();
    assert!(!names.is_empty(), "expected at least one tool");
    for name in &names {
        assert!(
            name == "cfgd" || name.starts_with("cfgd_"),
            "tool name must start with cfgd prefix, got {name}"
        );
    }
}

#[test]
fn every_tool_carries_a_safety_hint() {
    // A client decides whether to prompt from `readOnlyHint`, so a command
    // added to the CLI without a row in one of the classification tables is a
    // tool the client has to guess about. Fail here instead.
    let tools = all_tools();

    let unclassified: Vec<&str> = tools
        .iter()
        .filter(|t| {
            t.annotations
                .as_ref()
                .and_then(|a| a.read_only_hint)
                .is_none()
        })
        .map(|t| t.name.as_ref())
        .collect();

    assert!(
        unclassified.is_empty(),
        "every tool needs a readOnlyHint; add these to a table in cfgd::mcp::brontes: {unclassified:?}"
    );
}

#[test]
fn plan_and_apply_are_distinguishable() {
    let tools = all_tools();
    let hint = |name: &str| {
        tools
            .iter()
            .find(|t| t.name.as_ref() == name)
            .unwrap_or_else(|| panic!("expected tool {name}"))
            .annotations
            .clone()
            .unwrap_or_else(|| panic!("{name} must carry annotations"))
    };

    assert_eq!(
        hint("cfgd_plan").read_only_hint,
        Some(true),
        "plan previews and must be callable without a prompt"
    );
    assert_eq!(
        hint("cfgd_apply").read_only_hint,
        Some(false),
        "apply changes the machine"
    );
    assert_eq!(
        hint("cfgd_apply").destructive_hint,
        Some(true),
        "apply can replace files and restart services"
    );
}

#[test]
fn backup_run_is_annotated_destructive_and_non_idempotent() {
    // Retention pruning deletes superseded snapshots, the hooks stop and start
    // services, and a second identical call snapshots and prunes again — the
    // ADDITIVE table's "creates or updates without removing anything, safe to
    // repeat" contract is false for it, and a client trusting that would call
    // it without prompting.
    let tools = all_tools();
    let annotations = tools
        .iter()
        .find(|t| t.name.as_ref() == "cfgd_backup_run")
        .expect("expected tool cfgd_backup_run")
        .annotations
        .clone()
        .expect("cfgd_backup_run must carry annotations");

    assert_eq!(annotations.read_only_hint, Some(false));
    assert_eq!(
        annotations.destructive_hint,
        Some(true),
        "retention pruning removes snapshots and the hooks restart services"
    );
    assert_eq!(
        annotations.idempotent_hint,
        Some(false),
        "a second call takes another snapshot and prunes again"
    );
}

#[test]
fn hangs_and_nested_servers_are_not_tools() {
    let names = tool_names();
    for hidden in [
        // A tool that starts a second MCP server inside this one.
        "cfgd_mcp-server",
        // Runs in the foreground until killed.
        "cfgd_daemon_run",
        // Block on $EDITOR, which a tool call has no terminal for.
        "cfgd_config_edit",
        "cfgd_module_edit",
        "cfgd_profile_edit",
        "cfgd_secret_edit",
        "cfgd_source_edit",
        // Shell-prompt artifacts.
        "cfgd_man",
        "cfgd_completion",
    ] {
        assert!(
            !names.iter().any(|n| n == hidden),
            "{hidden} must stay out of the tool list"
        );
    }
}

#[test]
fn groups_slice_the_surface_into_usable_servers() {
    let cfg = mcp::config();
    let full = tool_names().len();

    for group in [
        "core",
        "sources",
        "modules",
        "profiles",
        "secrets",
        "authoring",
        "fleet",
        "image",
    ] {
        let tools = brontes::generate_tools(&Cli::command(), &cfg.clone().expose_group(group))
            .unwrap_or_else(|e| panic!("group {group} must resolve to a tool list: {e}"));
        assert!(
            !tools.is_empty(),
            "group {group} must select at least one tool"
        );
        assert!(
            tools.len() < full,
            "group {group} selected all {full} tools, which defeats the point of grouping"
        );
    }

    let core = brontes::generate_tools(&Cli::command(), &cfg.expose_group("core"))
        .expect("core group must resolve");
    let names: Vec<&str> = core.iter().map(|t| t.name.as_ref()).collect();
    assert!(
        names.contains(&"cfgd_apply") && names.contains(&"cfgd_plan"),
        "the core group must carry the reconcile commands, got: {names:?}"
    );
}

#[test]
fn a_server_started_with_no_flags_serves_the_core_group() {
    // The whole surface is more list than a client's context should spend on
    // one server, so the default is a slice. Which slice is a promise to
    // anyone who registered cfgd without selection flags.
    let shipped = shipped_tool_names();
    let core = brontes::generate_tools(&Cli::command(), &mcp::config().expose_group("core"))
        .expect("core group must resolve")
        .iter()
        .map(|t| t.name.to_string())
        .collect::<Vec<_>>();

    assert_eq!(shipped, core, "the shipped default must be the core group");
    assert!(
        shipped.len() < tool_names().len(),
        "the default must actually trim the surface"
    );
    assert!(
        shipped.contains(&"cfgd_apply".to_string()),
        "reconciling a machine is what cfgd is for: {shipped:?}"
    );
}
