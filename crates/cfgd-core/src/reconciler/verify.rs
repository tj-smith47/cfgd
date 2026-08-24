use serde::Serialize;

use crate::config::{EnvScope, LOCAL_LAYER, ResolvedProfile};
use crate::errors::Result;
use crate::expand_tilde;
use crate::modules::ResolvedModule;
use crate::providers::ProviderRegistry;
use crate::state::StateStore;
use crate::to_posix_string;

use super::env_engine::{
    EnvContent, EnvHostProbe, EnvOrigins, EnvPlatform, EnvTarget, env_targets,
};

/// Record a drift event or log a warning if the write fails. Previous sites
/// used `.ok()` which silently dropped SQLite errors (locked DB, full disk),
/// leaving `unresolved_drift()` out of sync with observed reality.
pub(super) fn record_drift_or_warn(
    state: &StateStore,
    resource_type: &str,
    resource_id: &str,
    expected: Option<&str>,
    actual: Option<&str>,
    source: &str,
) {
    if let Err(e) = state.record_drift(resource_type, resource_id, expected, actual, source) {
        tracing::warn!(
            error = %e,
            resource_type = %resource_type,
            resource_id = %resource_id,
            "failed to record drift"
        );
    }
}

/// Verify all managed resources match their desired state.
///
/// `cx` is the caller's package context rather than one built here, so the
/// manager half of `cfgd verify` — planned separately, in the CLI — reads the
/// same enumeration this function does instead of walking every manager a
/// second time.
pub fn verify(
    resolved: &ResolvedProfile,
    registry: &ProviderRegistry,
    state: &StateStore,
    modules: &[ResolvedModule],
    cx: &crate::providers::PackageContext<'_>,
) -> Result<Vec<VerifyResult>> {
    let mut results = Vec::new();

    // Verify packages — profile and module packages share one effective desired
    // set so a `(manager, name)` declared in both is checked once, and the
    // module-vs-profile attribution drives the result shape.
    let available_managers = registry.available_package_managers();
    for ep in crate::effective::effective_desired_packages(&resolved.merged, modules) {
        // A `prefer: [script]` package has no queryable installed-state: a custom
        // install script can put anything anywhere, so there is no
        // installed_packages() set to diff it against. It is therefore invisible
        // to drift detection by design. Idempotency for these installs is the
        // script's responsibility, expressed via the package entry's
        // creates/onlyIf/unless guards (honored on the apply path in
        // reconciler::modules) — not something verify can re-derive here.
        if ep.manager == "script" {
            continue;
        }

        // A manager that isn't available on this host cannot install or report
        // its packages, so a "missing" verdict here would be a false alarm. Skip
        // such packages for BOTH origins (profile packages were already skipped
        // by iterating only available managers; module packages used to be
        // reported missing — this makes the two consistent).
        let Some(mgr) = available_managers.iter().find(|m| m.name() == ep.manager) else {
            continue;
        };

        // Compare through package_identity so case-insensitive managers (choco/scoop/
        // winget: `wget` vs installed `Wget`) and name-remapping managers (go: module
        // path vs binary) match like with like.
        let ok = cx
            .installed_for(*mgr)?
            .contains(&mgr.package_identity(&ep.name));

        // Preserve each origin's resource conventions: module packages report as
        // `module` / `<module>/<name>`; profile packages as `package` /
        // `<manager>:<name>`.
        let (resource_type, resource_id) = match &ep.origin {
            crate::effective::Origin::Module(name) => ("module", format!("{}/{}", name, ep.name)),
            crate::effective::Origin::Profile => ("package", format!("{}:{}", ep.manager, ep.name)),
        };

        results.push(VerifyResult {
            resource_type: resource_type.to_string(),
            resource_id: resource_id.clone(),
            matches: ok,
            expected: "installed".to_string(),
            actual: if ok {
                "installed".to_string()
            } else {
                "missing".to_string()
            },
        });

        if !ok {
            record_drift_or_warn(
                state,
                resource_type,
                &resource_id,
                Some("installed"),
                Some("missing"),
                LOCAL_LAYER,
            );
        }
    }

    // Verify system configurators against the effective (profile ⊕ modules)
    // system map so module system config is verified too.
    let system = crate::effective::effective_system_map(&resolved.merged, modules);
    for sc in registry.available_system_configurators() {
        if let Some(desired) = system.get(sc.name()) {
            let drifts = sc.diff(desired)?;
            if drifts.is_empty() {
                results.push(VerifyResult {
                    resource_type: "system".to_string(),
                    resource_id: sc.name().to_string(),
                    matches: true,
                    expected: "configured".to_string(),
                    actual: "configured".to_string(),
                });
            } else {
                for drift in &drifts {
                    results.push(VerifyResult {
                        resource_type: "system".to_string(),
                        resource_id: format!("{}.{}", sc.name(), drift.key),
                        matches: false,
                        expected: drift.expected.clone(),
                        actual: drift.actual.clone(),
                    });

                    record_drift_or_warn(
                        state,
                        "system",
                        &format!("{}.{}", sc.name(), drift.key),
                        Some(&drift.expected),
                        Some(&drift.actual),
                        LOCAL_LAYER,
                    );
                }
            }
        }
    }

    // Managed-file verification is content-aware and lives in the binary crate
    // (`cli::live_drift`), which can reach `CfgdFileManager` to compare rendered
    // source bytes against the on-disk target. This reconciler cannot — the file
    // manager is across the crate boundary — so file results are folded in by the
    // caller rather than computed here as a presence-only check.

    // Verify env: re-derive the same targets the planner wrote and check each.
    let path_dirs = super::env::recorded_manager_path_dirs(state, &resolved.merged, modules);
    verify_env(
        &resolved.merged.env,
        &resolved.merged.aliases,
        resolved.merged.env_scope,
        modules,
        &path_dirs,
        state,
        &mut results,
    );

    Ok(results)
}

/// Result of verifying a single resource.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyResult {
    pub resource_type: String,
    pub resource_id: String,
    pub matches: bool,
    pub expected: String,
    pub actual: String,
}

/// Merge every module's `env`/`aliases` over the profile's, and record which
/// module each merged entry came from.
///
/// The origins travel with the merge because they are decided BY it: a later
/// module overriding an earlier one owns the value that survives, and the same
/// last-writer rule has to answer "whose is this" or the comment beside a line
/// names a module whose value is not there. Both surfaces that derive env-file
/// content call this — the planner and `verify` — so the two cannot disagree
/// about a file's bytes and report the difference as permanent drift.
pub(super) fn merge_module_env_aliases(
    profile_env: &[crate::config::EnvVar],
    profile_aliases: &[crate::config::ShellAlias],
    modules: &[ResolvedModule],
) -> (
    Vec<crate::config::EnvVar>,
    Vec<crate::config::ShellAlias>,
    EnvOrigins,
) {
    let mut merged = profile_env.to_vec();
    let mut merged_aliases = profile_aliases.to_vec();
    let mut origins = EnvOrigins::default();
    for module in modules {
        crate::merge_env(&mut merged, &module.env);
        crate::merge_aliases(&mut merged_aliases, &module.aliases);
        origins.claim(module);
    }
    (merged, merged_aliases, origins)
}

/// Verify env file and shell rc source line match expected state, persisting
/// a drift record for every non-matching result [`env_verify_results`]
/// computes. The compute/persist split is what lets `cli::live_drift` (the
/// shared engine behind `status --scan` and the non-recording half of
/// `verify`) run the identical checks read-only.
// NOTE: Secret-backed env vars (from SecretSpec.envs) are not included in
// verification because they require provider resolution. This means cfgd status
// may report env file drift after secret envs are written. This will be addressed
// when compliance snapshots track secret env metadata.
pub(super) fn verify_env(
    profile_env: &[crate::config::EnvVar],
    profile_aliases: &[crate::config::ShellAlias],
    scope: EnvScope,
    modules: &[ResolvedModule],
    path_dirs: &[String],
    state: &StateStore,
    results: &mut Vec<VerifyResult>,
) {
    for r in env_verify_results(profile_env, profile_aliases, scope, modules, path_dirs) {
        if !r.matches {
            record_drift_or_warn(
                state,
                &r.resource_type,
                &r.resource_id,
                Some(&r.expected),
                Some(&r.actual),
                LOCAL_LAYER,
            );
        }
        results.push(r);
    }
}

/// Pure computation behind [`verify_env`]: re-derive the exact env targets
/// the planner would write for this scope and check each against what is
/// actually on disk. Never touches the state store — the entry point for a
/// caller that wants the same checks without persisting a drift record
/// (`cli::live_drift`'s shared `status --scan` / `verify` engine).
pub fn env_verify_results(
    profile_env: &[crate::config::EnvVar],
    profile_aliases: &[crate::config::ShellAlias],
    scope: EnvScope,
    modules: &[ResolvedModule],
    path_dirs: &[String],
) -> Vec<VerifyResult> {
    let mut results = Vec::new();
    let (merged, merged_aliases, origins) =
        merge_module_env_aliases(profile_env, profile_aliases, modules);

    if merged.is_empty() && merged_aliases.is_empty() && path_dirs.is_empty() {
        return results;
    }

    // Re-derive the exact target set the planner wrote, so verify never reports
    // a file the current scope intentionally left unwritten as drift.
    let home = expand_tilde(std::path::Path::new("~"));
    let probe = EnvHostProbe::detect(&home);
    let platform = EnvPlatform::current();
    // `env_targets` always pushes the primary managed file (bash/zsh's
    // `.cfgd.env`, PowerShell's `.cfgd-env.ps1`) first when there is anything
    // to write, so the first `ManagedFile` this loop sees is the one file
    // whose dialect `verify_env_items` is built to read.
    let mut primary_checked = false;
    for target in env_targets(
        EnvContent::new(&merged, &merged_aliases, path_dirs, &origins),
        scope,
        &home,
        &probe,
        platform,
    ) {
        match target {
            EnvTarget::ManagedFile { path, content, .. } => {
                if !primary_checked {
                    primary_checked = true;
                    verify_env_items(
                        &path,
                        &merged,
                        &merged_aliases,
                        &origins,
                        platform,
                        &mut results,
                    );
                }
                verify_env_file(&path, &content, &mut results);
            }
            EnvTarget::SourceLine { rc_path, line } => {
                let has_line = std::fs::read_to_string(&rc_path)
                    .map(|content| content.contains(&line))
                    .unwrap_or(false);
                results.push(VerifyResult {
                    resource_type: "env-rc".to_string(),
                    resource_id: to_posix_string(&rc_path),
                    matches: has_line,
                    expected: "source line present".to_string(),
                    actual: if has_line {
                        "source line present".to_string()
                    } else {
                        "source line missing".to_string()
                    },
                });
            }
            // The live-session refresh is best-effort and ephemeral (a re-login
            // clears it); it is not a verified-drift surface — the durable file
            // targets above are authoritative.
            EnvTarget::LiveSession { .. } => {}
        }
    }
    results
}

/// Per-declared-item drift for the primary managed env file: whether the
/// line each declared alias and env var renders as is still present in what
/// is actually on disk. Follows the same read-the-generated-file-and-compare
/// pattern `verify_env_file` uses for the file as a whole, at row
/// granularity, so a status/diff consumer can attribute a mismatch to the
/// specific alias or env var that produced it instead of only "the file is
/// stale". A missing or unreadable file is left to the whole-file check this
/// function's caller also runs, which already reports it once at file
/// granularity.
fn verify_env_items(
    path: &std::path::Path,
    env: &[crate::config::EnvVar],
    aliases: &[crate::config::ShellAlias],
    origins: &EnvOrigins,
    platform: EnvPlatform,
    results: &mut Vec<VerifyResult>,
) {
    let Ok(actual) = std::fs::read_to_string(path) else {
        return;
    };
    let actual_lines: std::collections::HashSet<&str> = actual.lines().collect();

    for ev in env {
        let Some(line) = super::env_files::primary_env_var_line(ev, platform, origins) else {
            continue;
        };
        // Line-anchored, not a substring search: `actual.contains(&line)` would
        // read a commented-out `# export EDITOR="nvim"` as present, since the
        // declared line is a substring of the commented one.
        let matches = actual_lines.contains(line.as_str());
        results.push(VerifyResult {
            resource_type: "env-var".to_string(),
            resource_id: ev.name.clone(),
            matches,
            // Opaque markers, not the rendered line: the line is the user's own
            // declared value (`export EDITOR="nvim"`), and this result flows
            // unmodified into `drift_events` (`record_drift_or_warn`, below) and
            // the device gateway. A display surface that wants the actual line
            // recomputes it from the declared config at render time — see
            // `env_item_declared_line`.
            expected: "current".to_string(),
            actual: if matches {
                "current".to_string()
            } else {
                "missing or changed".to_string()
            },
        });
    }

    for alias in aliases {
        let Some(line) = super::env_files::primary_alias_line(alias, platform, origins) else {
            continue;
        };
        let matches = actual_lines.contains(line.as_str());
        results.push(VerifyResult {
            resource_type: "alias".to_string(),
            resource_id: alias.name.clone(),
            matches,
            expected: "current".to_string(),
            actual: if matches {
                "current".to_string()
            } else {
                "missing or changed".to_string()
            },
        });
    }
}

/// The line a declared env var or alias renders as, for a DISPLAY surface that
/// wants to show a drifted item's real value rather than the opaque
/// `current`/`missing or changed` markers [`verify_env_items`] returns. Never
/// called from a path that persists or ships its result: that is exactly the
/// content the opaque markers exist to keep out of `drift_events` and the
/// device gateway. `resource_type` is `"env-var"` or `"alias"`; any other kind
/// (or an item no longer declared) answers `None`.
pub fn env_item_declared_line(
    resource_type: &str,
    resource_id: &str,
    env: &[crate::config::EnvVar],
    aliases: &[crate::config::ShellAlias],
    modules: &[ResolvedModule],
) -> Option<String> {
    let platform = EnvPlatform::current();
    // The same merge the write and the verify pass ran, so the line shown is
    // the line the file must hold — including the ` # module:<name>` comment a
    // module-declared entry carries, which is part of what verify matched on.
    // A module's entries are not in `env`/`aliases` at all (those are the
    // profile's own), so without the merge a module-owned row could only ever
    // answer `None`.
    // Answered before the merge, not inside it: the merge clones the profile's
    // env and aliases and folds every resolved module in, and a caller looping
    // over a whole drift report would pay that per file/package/system row only
    // to be told the kind has no declared line at all.
    if !matches!(resource_type, "env-var" | "alias") {
        return None;
    }
    let (merged, merged_aliases, origins) = merge_module_env_aliases(env, aliases, modules);
    match resource_type {
        "env-var" => merged
            .iter()
            .find(|e| e.name == resource_id)
            .and_then(|e| super::env_files::primary_env_var_line(e, platform, &origins)),
        _ => merged_aliases
            .iter()
            .find(|a| a.name == resource_id)
            .and_then(|a| super::env_files::primary_alias_line(a, platform, &origins)),
    }
}

/// The line the primary managed env file ACTUALLY holds for `resource_id`
/// right now: the deployed line whose dialect-rendered prefix the CURRENT
/// declaration of that name claims, so a hand-edited value is recognized as
/// this item's line rather than as a stranger's. `None` when nothing on disk
/// claims the name — either because the file is not there at all, or because
/// nothing in it starts with the prefix the declaration renders.
///
/// `Err` is reserved for "I could not look": a file that exists and cannot be
/// read (a lost `+r`, a sharing violation) says nothing about whether the entry
/// is deployed, and answering `Missing` there would report an absence the
/// machine never confirmed. Only `NotFound` reads as absent.
fn deployed_env_item_line(
    resource_type: &str,
    resource_id: &str,
) -> std::io::Result<Option<String>> {
    let platform = EnvPlatform::current();
    let claims: Vec<String> = match resource_type {
        "env-var" => super::env_files::env_var_line_prefix(resource_id, platform)
            .into_iter()
            .collect(),
        "alias" => super::env_files::alias_line_prefixes(resource_id, platform),
        _ => return Ok(None),
    };
    let home = expand_tilde(std::path::Path::new("~"));
    let content =
        match std::fs::read_to_string(super::env_engine::primary_env_file_path(&home, platform)) {
            Ok(content) => content,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
    Ok(content
        .lines()
        .find(|line| claims.iter().any(|p| line.starts_with(p.as_str())))
        .map(|line| line.trim_end().to_string()))
}

/// The DISPLAY `(want, have)` pair for one env-var/alias row, recomputed from
/// the machine: `want` is the line the current declaration renders as
/// ([`env_item_declared_line`]), `have` is the line the managed file actually
/// holds ([`deployed_env_item_line`]), or [`crate::Absence::Missing`] when no
/// deployed line claims the name. `None` for any other resource kind, for an
/// item no longer declared, and for a managed file that exists but could not be
/// read — the caller keeps the operands it already has. Being unable to LOOK is
/// not the same fact as the entry being gone, and only the second may be
/// reported as an absence.
///
/// This is the one place that recompute happens, so `diff`, `verify`,
/// `status` and `status --scan` cannot word the same env var four ways. Same
/// "never call this on a value about to be persisted or shipped to the
/// gateway" rule as `env_item_declared_line`: both halves are real values,
/// which is exactly what the opaque `current` / `missing or changed` markers
/// exist to keep out of `drift_events`.
pub fn env_item_display_values(
    resource_type: &str,
    resource_id: &str,
    env: &[crate::config::EnvVar],
    aliases: &[crate::config::ShellAlias],
    modules: &[ResolvedModule],
) -> Option<(String, String)> {
    let declared = env_item_declared_line(resource_type, resource_id, env, aliases, modules)?;
    let deployed = match deployed_env_item_line(resource_type, resource_id) {
        Ok(Some(line)) => line,
        Ok(None) => crate::Absence::Missing.as_str().to_string(),
        Err(_) => return None,
    };
    Some((declared, deployed))
}

/// Verify a single env file's content matches expected.
pub(super) fn verify_env_file(
    path: &std::path::Path,
    expected: &str,
    results: &mut Vec<VerifyResult>,
) {
    match std::fs::read_to_string(path) {
        Ok(actual) if actual == expected => {
            results.push(VerifyResult {
                resource_type: "env".to_string(),
                resource_id: to_posix_string(path),
                matches: true,
                expected: "current".to_string(),
                actual: "current".to_string(),
            });
        }
        Ok(_) => {
            results.push(VerifyResult {
                resource_type: "env".to_string(),
                resource_id: to_posix_string(path),
                matches: false,
                expected: "current".to_string(),
                actual: "stale".to_string(),
            });
        }
        Err(_) => {
            results.push(VerifyResult {
                resource_type: "env".to_string(),
                resource_id: to_posix_string(path),
                matches: false,
                expected: "present".to_string(),
                actual: "missing".to_string(),
            });
        }
    }
}
