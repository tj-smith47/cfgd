use crate::PathDisplayExt;
use crate::providers::{FileAction, PackageAction, SecretAction};
use crate::to_posix_string;

use super::types::{
    Action, EnvAction, ModuleAction, ModuleActionKind, OwnerGroup, ScriptAction, SystemAction,
};

/// Resource id of the live-session env refresh. The planner and
/// `apply_env_action` must both emit it verbatim: it is the only env surface
/// with no path to key on, so a divergence between the two makes the applied
/// result unmatchable against the action that planned it.
pub(super) const LIVE_SESSION_RESOURCE_ID: &str = "env:session:refresh";

/// Append source provenance suffix for non-local origins.
pub(super) fn provenance_suffix(origin: &str) -> String {
    if origin.is_empty() || origin == "local" {
        String::new()
    } else {
        format!(" <- {origin}")
    }
}

/// Format a canonical description of an action.
///
/// Used as the SQLite `managed_resource` resource_id, the
/// `ActionResult.description` JSON field, AND the user-facing apply-error
/// printer line. Embedded paths are always folded to POSIX form via
/// `to_posix_string` so the same logical resource carries the same key on
/// every OS — drift correlation, JSON wire form, and human display all
/// agree.
pub fn format_action_description(action: &Action) -> String {
    let path_str = to_posix_string;
    match action {
        Action::File(fa) => match fa {
            FileAction::Create { target, .. } => format!("file:create:{}", path_str(target)),
            FileAction::Update { target, .. } => format!("file:update:{}", path_str(target)),
            FileAction::Delete { target, .. } => format!("file:delete:{}", path_str(target)),
            FileAction::SetPermissions { target, mode, .. } => {
                format!("file:chmod:{:#o}:{}", mode, path_str(target))
            }
            FileAction::Skip { target, .. } => format!("file:skip:{}", path_str(target)),
        },
        Action::Package(pa) => match pa {
            PackageAction::Bootstrap { manager, .. } => {
                format!("package:{}:bootstrap", manager)
            }
            PackageAction::Install {
                manager, packages, ..
            } => format!("package:{}:install:{}", manager, packages.join(",")),
            PackageAction::Uninstall {
                manager, packages, ..
            } => format!("package:{}:uninstall:{}", manager, packages.join(",")),
            PackageAction::Skip { manager, .. } => format!("package:{}:skip", manager),
        },
        Action::Secret(sa) => match sa {
            SecretAction::Decrypt {
                target, backend, ..
            } => format!("secret:decrypt:{}:{}", backend, path_str(target)),
            SecretAction::Resolve {
                provider,
                reference,
                target,
                ..
            } => format!(
                "secret:resolve:{}:{}:{}",
                provider,
                reference,
                path_str(target)
            ),
            SecretAction::ResolveEnv {
                provider,
                reference,
                envs,
                ..
            } => format!(
                "secret:resolve-env:{}:{}:[{}]",
                provider,
                reference,
                envs.join(",")
            ),
            SecretAction::Skip { source, .. } => format!("secret:skip:{}", source),
        },
        Action::System(sa) => match sa {
            SystemAction::SetValue {
                configurator, key, ..
            } => format!("system:{}.{}", configurator, key),
            SystemAction::Skip { configurator, .. } => {
                format!("system:{}:skip", configurator)
            }
        },
        Action::Script(sa) => match sa {
            // Resource-id / state-matching key, NOT a display string: this
            // return value is the SQLite `managed_resource` id and the
            // `ActionResult.description` JSON field. Condensing `run_str()`
            // here would reshape the id and break drift matching against
            // every already-recorded state row for a module with a
            // multi-line inline script — leave it byte-identical.
            ScriptAction::Run { entry, phase, .. } => {
                format!("script:{}:{}", phase.display_name(), entry.run_str())
            }
        },
        Action::Module(ma) => match &ma.kind {
            ModuleActionKind::InstallPackages { resolved } => {
                let names: Vec<&str> = resolved.iter().map(|p| p.resolved_name.as_str()).collect();
                format!("module:{}:packages:{}", ma.module_name, names.join(","))
            }
            ModuleActionKind::DeployFiles { files } => {
                format!("module:{}:files:{}", ma.module_name, files.len())
            }
            ModuleActionKind::RunScript { .. } => {
                format!("module:{}:script", ma.module_name)
            }
            ModuleActionKind::Skip { .. } => {
                format!("module:{}:skip", ma.module_name)
            }
        },
        Action::Env(ea) => match ea {
            EnvAction::WriteEnvFile { path, .. } => {
                format!("env:write:{}", path_str(path))
            }
            EnvAction::InjectSourceLine { rc_path, .. } => {
                format!("env:inject:{}", path_str(rc_path))
            }
            EnvAction::RefreshLiveSession { .. } => LIVE_SESSION_RESOURCE_ID.to_string(),
        },
    }
}

/// Condense `desc` for a status-subject/error-message/bullet display only
/// when `action` can embed a raw, potentially multi-line script body:
/// `format_action_description`'s `Action::Script` arm and
/// `format_module_action_body`'s `ModuleActionKind::RunScript` arm both embed
/// `entry.run_str()`/`script.run_str()` verbatim so `-o json` payloads and
/// `ActionResult.description` stay byte-identical to the source body. Every
/// other action kind's description is already condensed/newline-free.
/// Callers must keep the raw `desc` for `ActionResult.description` / journal
/// persistence / the `-o json` plan payload — this helper is display-only.
pub fn condense_action_desc_for_display(action: &Action, desc: &str) -> String {
    let embeds_raw_script = matches!(action, Action::Script(_))
        || matches!(
            action,
            Action::Module(ModuleAction {
                kind: ModuleActionKind::RunScript { .. },
                ..
            })
        );
    if embeds_raw_script {
        crate::output::condense_script_label(desc)
    } else {
        desc.to_string()
    }
}

/// Format one plan item for display.
pub fn format_plan_item(action: &Action) -> String {
    match action {
        Action::File(fa) => match fa {
            FileAction::Create { target, origin, .. } => {
                format!("create {}{}", target.posix(), provenance_suffix(origin))
            }
            FileAction::Update { target, origin, .. } => {
                format!("update {}{}", target.posix(), provenance_suffix(origin))
            }
            FileAction::Delete { target, origin, .. } => {
                format!("delete {}{}", target.posix(), provenance_suffix(origin))
            }
            FileAction::SetPermissions {
                target,
                mode,
                origin,
                ..
            } => format!(
                "chmod {:#o} {}{}",
                mode,
                target.posix(),
                provenance_suffix(origin)
            ),
            FileAction::Skip {
                target,
                reason,
                origin,
                ..
            } => format!(
                "skip {}: {}{}",
                target.posix(),
                reason,
                provenance_suffix(origin)
            ),
        },
        Action::Package(pa) => match pa {
            PackageAction::Bootstrap {
                manager,
                method,
                origin,
                ..
            } => format!(
                "bootstrap {} via {}{}",
                manager,
                method,
                provenance_suffix(origin)
            ),
            PackageAction::Install {
                manager,
                packages,
                origin,
                ..
            } => format!(
                "install via {}: {}{}",
                manager,
                packages.join(", "),
                provenance_suffix(origin)
            ),
            PackageAction::Uninstall {
                manager,
                packages,
                origin,
                ..
            } => format!(
                "uninstall via {}: {}{}",
                manager,
                packages.join(", "),
                provenance_suffix(origin)
            ),
            PackageAction::Skip {
                manager,
                reason,
                origin,
                ..
            } => format!("skip {}: {}{}", manager, reason, provenance_suffix(origin)),
        },
        Action::Secret(sa) => match sa {
            SecretAction::Decrypt {
                source,
                target,
                backend,
                origin,
                ..
            } => format!(
                "decrypt {} → {} (via {}){}",
                source.posix(),
                target.posix(),
                backend,
                provenance_suffix(origin)
            ),
            SecretAction::Resolve {
                provider,
                reference,
                target,
                origin,
                ..
            } => format!(
                "resolve {}://{} → {}{}",
                provider,
                reference,
                target.posix(),
                provenance_suffix(origin)
            ),
            SecretAction::ResolveEnv {
                provider,
                reference,
                envs,
                origin,
                ..
            } => format!(
                "resolve {}://{} → env [{}]{}",
                provider,
                reference,
                envs.join(", "),
                provenance_suffix(origin)
            ),
            SecretAction::Skip {
                source,
                reason,
                origin,
                ..
            } => format!("skip {}: {}{}", source, reason, provenance_suffix(origin)),
        },
        Action::System(sa) => match sa {
            SystemAction::SetValue {
                configurator,
                key,
                desired,
                current,
                origin,
                ..
            } => format!(
                "set {}.{}: {} → {}{}",
                configurator,
                key,
                current,
                desired,
                provenance_suffix(origin)
            ),
            SystemAction::Skip {
                configurator,
                reason,
                unknown,
                ..
            } => {
                if *unknown {
                    format!(
                        "unknown system key '{}' — no such configurator (ignored)",
                        configurator
                    )
                } else {
                    format!("skip {}: {}", configurator, reason)
                }
            }
        },
        Action::Script(sa) => match sa {
            ScriptAction::Run {
                entry,
                phase,
                origin,
                ..
            } => {
                // Raw body: this same `Vec<String>` feeds both
                // `display_plan_table`/`cli/apply.rs`'s dry-run preview
                // (human bullets) AND `build_plan_output`'s
                // `PlanActionOutput.description` (the `-o json` plan
                // payload). Condensing here would truncate the JSON
                // payload too — display sites condense for themselves via
                // `condense_action_desc_for_display`.
                format!(
                    "run {} script: {}{}",
                    phase.display_name(),
                    entry.run_str(),
                    provenance_suffix(origin)
                )
            }
        },
        Action::Module(ma) => format_module_action_item(ma),
        Action::Env(ea) => match ea {
            EnvAction::WriteEnvFile { path, .. } => {
                format!("write {}", path.posix())
            }
            EnvAction::InjectSourceLine { rc_path, .. } => {
                format!("inject source line into {}", rc_path.posix())
            }
            EnvAction::RefreshLiveSession { vars } => {
                format!("refresh live session ({} var(s))", vars.len())
            }
        },
    }
}

/// Format one owner group's plan items for display, one per action in order.
pub fn format_plan_items(group: &OwnerGroup) -> Vec<String> {
    group.actions.iter().map(format_plan_item).collect()
}

/// Format a module action for plan display.
///
/// Source-delivered modules (`origin = Some`) get the same ` <- <source>`
/// provenance suffix as source-delivered files/packages; consumer-local modules
/// (`origin = None`) render with no suffix.
pub(super) fn format_module_action_item(action: &ModuleAction) -> String {
    let suffix = provenance_suffix(action.origin.as_deref().unwrap_or(""));
    let body = format_module_action_body(action);
    format!("{body}{suffix}")
}

fn format_module_action_body(action: &ModuleAction) -> String {
    match &action.kind {
        ModuleActionKind::InstallPackages { resolved } => {
            // Group by manager in first-appearance order: this string is also
            // the persisted plan/description payload, so its manager segments
            // must be deterministic across runs (a HashMap here reshuffled
            // multi-manager modules on every plan).
            let mut by_manager: Vec<(&str, Vec<String>)> = Vec::new();
            for pkg in resolved {
                let display = if let Some(ref ver) = pkg.version {
                    if pkg.canonical_name != pkg.resolved_name {
                        format!(
                            "{} ({}, alias: {})",
                            pkg.resolved_name, ver, pkg.canonical_name
                        )
                    } else {
                        format!("{} ({})", pkg.resolved_name, ver)
                    }
                } else if pkg.canonical_name != pkg.resolved_name {
                    format!("{} (alias: {})", pkg.resolved_name, pkg.canonical_name)
                } else {
                    pkg.resolved_name.clone()
                };
                match by_manager.iter_mut().find(|(mgr, _)| *mgr == pkg.manager) {
                    Some((_, pkgs)) => pkgs.push(display),
                    None => by_manager.push((&pkg.manager, vec![display])),
                }
            }
            let parts: Vec<String> = by_manager
                .iter()
                .map(|(mgr, pkgs)| format!("{} install {}", mgr, pkgs.join(", ")))
                .collect();
            format!("[{}] {}", action.module_name, parts.join("; "))
        }
        ModuleActionKind::DeployFiles { files } => {
            let targets: Vec<String> = files.iter().map(|f| f.target.display_posix()).collect();
            if targets.len() <= 3 {
                format!("[{}] deploy: {}", action.module_name, targets.join(", "))
            } else {
                format!(
                    "[{}] deploy: {} ({} files)",
                    action.module_name,
                    targets[..2].join(", "),
                    targets.len()
                )
            }
        }
        ModuleActionKind::RunScript { script, phase } => {
            // Raw body: this same string feeds both
            // `display_plan_table`/`cli/apply.rs`'s dry-run preview (human
            // bullets) AND `build_plan_output`'s `PlanActionOutput.description`
            // (the `-o json` plan payload) via `format_plan_items` ->
            // `format_module_action_item`. Condensing here would truncate the
            // JSON payload too — display sites condense for themselves via
            // `condense_action_desc_for_display`.
            format!(
                "[{}] {}: {}",
                action.module_name,
                phase.display_name(),
                script.run_str()
            )
        }
        ModuleActionKind::Skip { reason } => {
            format!("[{}] skip: {}", action.module_name, reason)
        }
    }
}

/// Prefixes whose `format_action_description`/`execute_script` output has a
/// structural-colon count fixed at TWO (`type:subtype:body`) for every variant
/// of that action kind. Only for those is the second segment safely droppable:
/// it names the verb/phase that produced the row (`create`/`update`/`delete`
/// for `file`, `pre`/`post` for `script`), never an identity, and drift/state
/// matching keys on `(type, body)` so two verbs touching the same target share
/// one id.
///
/// Excluded prefixes either vary their structural-colon count or carry an
/// identity in the second segment, so no fixed split is correct for them:
/// - `module` names the MODULE in its second segment (`module:{name}:{verb}`),
///   not a verb — dropping it would collapse every module onto one
///   `UNIQUE(resource_type, resource_id)` row.
/// - `package` names the MANAGER (`package:{manager}:{verb}`) — same collapse:
///   `package:brew:skip` and `package:apt:skip` would share one row. Only
///   `bootstrap` and `skip` reach here; `install`/`uninstall` are handled
///   ahead of this parser, per-package, by `parse_package_description`.
/// - `system` stamps `system:{configurator}.{key}` (one colon) but
///   `system:{configurator}:skip` (two).
/// - `execute_script`'s `"Running script: {body}"` has one.
///
/// A blind `splitn(3, ':')` cannot tell "2 structural colons" apart from
/// "1 structural colon + a colon embedded in the body" — both consume 2 colons
/// and yield 3 pieces. Dispatching on the known prefix (rather than counting
/// colons) keeps the body intact either way: a `run:` script or `-o json` value
/// containing its own `:` no longer gets silently truncated mid-body.
const TWO_COLON_PREFIXES: &[&str] = &["file", "secret", "script", "env"];

pub(super) fn parse_resource_from_description(desc: &str) -> (String, String) {
    let Some((prefix, rest)) = desc.split_once(':') else {
        return ("unknown".to_string(), desc.to_string());
    };
    if TWO_COLON_PREFIXES.contains(&prefix) {
        let id = rest.split_once(':').map_or(rest, |(_, id)| id);
        (prefix.to_string(), id.to_string())
    } else {
        (prefix.to_string(), rest.to_string())
    }
}

/// Parse a package action description (`"package:<mgr>:<verb>:<csv-packages>"`)
/// into `(manager, verb, packages)`. Returns `None` for any description that is
/// not a package action or lacks a package list (e.g. `package:<mgr>:bootstrap`,
/// `package:<mgr>:skip`), so per-package tracking only fires for install/uninstall.
pub(super) fn parse_package_description(desc: &str) -> Option<(String, String, Vec<String>)> {
    let parts: Vec<&str> = desc.splitn(4, ':').collect();
    if parts.len() != 4 || parts[0] != "package" {
        return None;
    }
    let manager = parts[1].to_string();
    let verb = parts[2].to_string();
    let packages: Vec<String> = parts[3].split(',').map(str::to_string).collect();
    Some((manager, verb, packages))
}

#[cfg(test)]
mod tests {
    use super::parse_package_description;

    #[test]
    fn parse_package_install_single() {
        let parsed = parse_package_description("package:apt:install:hello").unwrap();
        assert_eq!(parsed.0, "apt");
        assert_eq!(parsed.1, "install");
        assert_eq!(parsed.2, vec!["hello".to_string()]);
    }

    #[test]
    fn parse_package_install_csv_multi() {
        let parsed =
            parse_package_description("package:cargo:install:bat,ripgrep,fd-find").unwrap();
        assert_eq!(parsed.0, "cargo");
        assert_eq!(parsed.1, "install");
        assert_eq!(
            parsed.2,
            vec![
                "bat".to_string(),
                "ripgrep".to_string(),
                "fd-find".to_string()
            ]
        );
    }

    #[test]
    fn parse_package_uninstall() {
        let parsed = parse_package_description("package:apt:uninstall:fd-find").unwrap();
        assert_eq!(parsed.1, "uninstall");
        assert_eq!(parsed.2, vec!["fd-find".to_string()]);
    }

    #[test]
    fn parse_package_bootstrap_and_skip_have_no_packages() {
        // bootstrap/skip descriptions carry no csv package list, so parsing
        // declines them — they never drive per-package tracking.
        assert!(parse_package_description("package:brew:bootstrap").is_none());
        assert!(parse_package_description("package:apt:skip").is_none());
    }

    #[test]
    fn parse_non_package_description_declines() {
        assert!(parse_package_description("file:create:/home/.zshrc").is_none());
    }
}
