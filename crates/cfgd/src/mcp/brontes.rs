//! The MCP tool surface cfgd exposes through brontes.
//!
//! cfgd's clap tree walks out to ~90 tools, which is more than a client can
//! usefully hold in one `tools/list`. The groups below let a server be
//! launched against the slice of cfgd a session actually needs
//! (`cfgd mcp start --group modules`), and the hints let a client tell
//! `cfgd plan` apart from `cfgd apply` without reading prose.

use brontes::{Config, TaskMode, ToolAnnotations};

/// Commands that read state and change nothing.
const READ_ONLY: &[&str] = &[
    "alias list",
    "alias show",
    "clusterconfigpolicy validate",
    "compliance",
    "compliance diff",
    "compliance history",
    "config get",
    "daemon",
    "daemon status",
    "config show",
    "configpolicy validate",
    "diff",
    "doctor",
    "explain",
    "generate",
    "log",
    "machineconfig validate",
    "module list",
    "module registry list",
    "module show",
    "module validate",
    "module keys list",
    "paths",
    "plan",
    "profile list",
    "profile show",
    "profile validate",
    "skill list",
    "source list",
    "source show",
    "source validate",
    "status",
    "verify",
];

/// Read-only commands that reach a registry, a gateway or a git remote.
const READ_ONLY_NETWORKED: &[&str] = &["module search"];

/// Commands that block on `$EDITOR`. A tool call has no terminal to hand the
/// editor, so each would hang until the client gave up.
const INTERACTIVE: &[&str] = &[
    "config edit",
    "module edit",
    "profile edit",
    "secret edit",
    "source edit",
];

/// Commands that create or update local state without removing anything.
const ADDITIVE: &[&str] = &[
    "alias set",
    "compliance export",
    "config set",
    "daemon install",
    "generate module",
    "generate profile",
    "init",
    "module create",
    "module export",
    "module keys generate",
    "module registry add",
    "module registry rename",
    "profile create",
    "profile migrate",
    "profile update",
    "profile switch",
    "secret encrypt",
    "secret init",
    "skill install",
    "upgrade",
    "workflow generate",
];

/// Commands that create or update state by talking to a remote.
const ADDITIVE_NETWORKED: &[&str] = &[
    "checkin",
    "decide",
    "enroll",
    "image pack",
    "module build",
    "module pull",
    "module push",
    "module update",
    "module upgrade",
    "pull",
    "skill update",
    "source add",
    "source create",
    "source override",
    "source priority",
    "source replace",
    "source update",
    "sync",
];

/// Commands that remove or overwrite state a user could not trivially
/// reconstruct.
const DESTRUCTIVE: &[&str] = &[
    "alias delete",
    "config unset",
    "module delete",
    "module keys rotate",
    "module registry remove",
    "daemon uninstall",
    "profile delete",
    "rollback",
    "secret decrypt",
    "skill remove",
    "source remove",
    // Drops managed-resource rows, so a later apply no longer knows it owns
    // what those rows tracked — irreversible without a re-apply.
    "state forget-prefix",
];

/// Commands that routinely run for minutes, or wait on a remote. Handed back
/// as task handles so a client can poll, stream progress and cancel instead
/// of holding one request open for the whole reconcile.
const LONG_RUNNING: &[&str] = &[
    "apply",
    "daemon install",
    "daemon uninstall",
    "image pack",
    "module build",
    "module pull",
    "module push",
    "module upgrade",
    "pull",
    "sync",
    "upgrade",
];

/// Group name → the top-level commands it covers. Membership reaches a
/// path's descendants, so naming `module` takes the whole `module` subtree.
const GROUPS: &[(&str, &str, &[&str])] = &[
    (
        "core",
        "Reconcile this machine: preview, apply, inspect drift, roll back",
        &[
            "apply", "plan", "status", "diff", "verify", "log", "rollback", "doctor", "paths",
        ],
    ),
    (
        "sources",
        "Where config comes from: remotes, sync, pending decisions, upgrades",
        &["source", "sync", "pull", "decide", "upgrade"],
    ),
    (
        "modules",
        "Author, publish and consume cfgd modules",
        &["module"],
    ),
    (
        "profiles",
        "Per-machine profile selection and authoring",
        &["profile"],
    ),
    (
        "secrets",
        "Encrypt and decrypt sops-managed secrets",
        &["secret"],
    ),
    (
        "authoring",
        "Write cfgd resources: scaffolding, CRD validation, schema docs, aliases, skills",
        &[
            "init",
            "generate",
            "explain",
            "machineconfig",
            "configpolicy",
            "clusterconfigpolicy",
            "skill",
            "alias",
            "config",
            "workflow",
        ],
    ),
    (
        "fleet",
        "Device-gateway enrollment, daemon lifecycle and compliance evidence",
        &["daemon", "checkin", "enroll", "compliance"],
    ),
    (
        "image",
        "Pack a host directory into an OCI image",
        &["image"],
    ),
];

/// Build the brontes configuration for cfgd's `mcp` subtree.
pub fn config() -> Config {
    let mut cfg = Config::default().tool_name_prefix("cfgd");

    // The bare root prints help. Annotated on its own rather than through
    // READ_ONLY because group membership covers a path's descendants, and the
    // root's descendants are the entire tree.
    cfg = cfg.annotation("cfgd", read(false));

    for path in READ_ONLY {
        cfg = cfg.annotation(*path, read(false));
    }
    for path in READ_ONLY_NETWORKED {
        cfg = cfg.annotation(*path, read(true));
    }
    for path in ADDITIVE {
        cfg = cfg.annotation(*path, write(false, true, false));
    }
    for path in ADDITIVE_NETWORKED {
        cfg = cfg.annotation(*path, write(false, true, true));
    }
    for path in DESTRUCTIVE {
        cfg = cfg.annotation(*path, write(true, false, false));
    }

    // `apply` is the command that changes the machine. It converges rather
    // than stacking, but it can replace files and restart services, so it
    // carries the destructive hint a client should prompt on.
    cfg = cfg.annotation("apply", write(true, true, true));

    for path in LONG_RUNNING {
        cfg = cfg.task_mode_for(*path, TaskMode::Detached);
    }

    // `mcp-server` is cfgd's own hand-rolled MCP server; exposing it as a
    // tool invites a client to start a second server inside this one.
    // `daemon run` runs in the foreground until killed, and `man` /
    // `completion` are shell-prompt artifacts.
    cfg = cfg
        .hide_command("mcp-server")
        .hide_command("daemon run")
        .hide_command("man")
        .hide_command("completion");
    for path in INTERACTIVE {
        cfg = cfg.hide_command(*path);
    }

    for (name, description, commands) in GROUPS {
        cfg = cfg
            .group(*name, commands.iter().copied())
            .group_description(*name, *description);
    }

    // 86 tools is more list than a client's context should spend on one
    // server, and the majority of it is authoring surface a session that
    // wants to reconcile a machine will never call. `core` is the slice
    // everyone needs; `--group`/`--command`/`--tool` widen it and `--all`
    // drops the pin entirely, so nothing here is reachable only by editing
    // this file.
    cfg.expose_group("core")
}

/// A read-only hint set. `destructive` and `idempotent` are left unset
/// because the spec only reads them when `read_only_hint` is not `true`.
fn read(open_world: bool) -> ToolAnnotations {
    ToolAnnotations {
        read_only_hint: Some(true),
        open_world_hint: Some(open_world),
        ..Default::default()
    }
}

/// A writing hint set: `idempotent` says whether a second identical call is a
/// no-op.
fn write(destructive: bool, idempotent: bool, open_world: bool) -> ToolAnnotations {
    ToolAnnotations {
        read_only_hint: Some(false),
        destructive_hint: Some(destructive),
        idempotent_hint: Some(idempotent),
        open_world_hint: Some(open_world),
        ..Default::default()
    }
}
