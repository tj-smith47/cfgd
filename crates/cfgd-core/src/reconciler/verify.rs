use serde::Serialize;

use crate::config::{EnvScope, ResolvedProfile};
use crate::errors::Result;
use crate::expand_tilde;
use crate::modules::ResolvedModule;
use crate::providers::ProviderRegistry;
use crate::state::StateStore;
use crate::to_posix_string;

use super::env_engine::{
    EnvContent, EnvHostProbe, EnvOrigins, EnvPlatform, EnvTarget, ManagerPathDir, env_targets,
};

/// Verify all managed resources match their desired state.
///
/// Pure compute over the machine: nothing here writes `drift_events`. The
/// caller decides which results become recorded rows — a FULL-machine check
/// records every finding, a `--module` one only the rows in its own scope —
/// and a recorder buried in here forced every scope to record machine-wide
/// (`cli::live_drift`'s module doc states the contract).
///
/// `cx` is the caller's package context rather than one built here, so the
/// manager half of `cfgd verify` — planned separately, in the CLI — reads the
/// same enumeration this function does instead of walking every manager a
/// second time.
///
/// `machine_surfaces` gates the system and env halves, which compare
/// MACHINE-wide surfaces (the live configurator state, the deployed env
/// files) against the composed desired state. A module-scoped caller passes
/// `false`: its composition holds module-only config, and diffing that
/// against a machine-wide surface produces a claim about the machine no
/// single module can vouch for — the same scope rule the CLI's recording
/// seam applies to what it stores.
pub fn verify(
    resolved: &ResolvedProfile,
    registry: &ProviderRegistry,
    state: &StateStore,
    modules: &[ResolvedModule],
    cx: &crate::providers::PackageContext<'_>,
    machine_surfaces: bool,
) -> Result<VerifyReport> {
    let mut results = Vec::new();
    let mut check_errors = Vec::new();

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

        // ONE identity per (manager, package), whichever origin declared it:
        // `package` / `<manager>:<name>`, the same key every CLI live check
        // mints (`cli::diff::package_resource_id`). A module-qualified key
        // here made `verify` and `diff`/`status --scan` record two rows for
        // one missing package — and resolve each other's as healed. The
        // module attribution stays an ORIGIN fact (`ep.origin`), not part of
        // the row's identity.
        results.push(VerifyResult {
            resource_type: "package".to_string(),
            resource_id: format!("{}:{}", ep.manager, ep.name),
            matches: ok,
            expected: "installed".to_string(),
            actual: if ok {
                "installed".to_string()
            } else {
                // The stored literal for a missing package is the ONE
                // `Absence::NotInstalled` spelling every producer of this
                // row shape writes (`cli::live_drift` mints the same), so a
                // reader folding recorded operands never meets two words for
                // one fact.
                crate::Absence::NotInstalled.as_str().to_string()
            },
            unmanaged: false,
        });
    }

    if !machine_surfaces {
        return Ok(VerifyReport {
            results,
            check_errors,
        });
    }

    // Verify system configurators against the effective (profile ⊕ modules)
    // system map so module system config is verified too. A configurator
    // whose own probe fails becomes DATA, not an abort: the remaining folds,
    // the recording and the scan stamp all still happen at the caller, and
    // the errored configurator contributes no system row — so its recorded
    // rows stand rather than being healed by a check that never ran.
    let system = crate::effective::effective_system_map(&resolved.merged, modules);
    for sc in registry.available_system_configurators() {
        if let Some(desired) = system.get(sc.name()) {
            let drifts = match sc.diff(desired) {
                Ok(drifts) => drifts,
                Err(e) => {
                    check_errors.push(SystemCheckError {
                        key: sc.name().to_string(),
                        error: crate::output::collapse_to_subject_line(e),
                    });
                    continue;
                }
            };
            if drifts.is_empty() {
                results.push(VerifyResult {
                    resource_type: "system".to_string(),
                    resource_id: sc.name().to_string(),
                    matches: true,
                    expected: "configured".to_string(),
                    actual: "configured".to_string(),
                    unmanaged: false,
                });
            } else {
                for drift in &drifts {
                    results.push(VerifyResult {
                        resource_type: "system".to_string(),
                        resource_id: format!("{}.{}", sc.name(), drift.key),
                        matches: false,
                        expected: drift.expected.clone(),
                        actual: drift.actual.clone(),
                        unmanaged: false,
                    });
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
    results.extend(env_verify_results(
        &resolved.merged.env,
        &resolved.merged.aliases,
        &resolved.merged.entry_owners,
        resolved.merged.env_scope,
        modules,
        &path_dirs,
    ));

    Ok(VerifyReport {
        results,
        check_errors,
    })
}

/// What a verify pass computed: one verdict per resource, plus the checks
/// that could not run. A check error travels as data rather than aborting,
/// so the caller still renders every other finding, records what WAS
/// checked, and escalates its exit to `Error` — the same first-class shape
/// `cli::live_drift`'s engine reports for `diff` and `status --scan`.
pub struct VerifyReport {
    pub results: Vec<VerifyResult>,
    pub check_errors: Vec<SystemCheckError>,
}

/// A configurator whose drift check itself failed — the machine's state for
/// that key is unknown, which no drifted/clean verdict may stand in for.
/// Serialized under `systemErrors` by every surface's `-o json` payload
/// (`diff`, `status`, `verify`), so an empty drift list beside a non-empty
/// `systemErrors` reads as "unknown", never "clean".
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemCheckError {
    pub key: String,
    pub error: String,
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
    /// Whether a `file` result's target holds a file cfgd never wrote. Appended
    /// last and `false` for every other resource type, so an existing reader
    /// sees the payload it always saw with one field added at the end.
    pub unmanaged: bool,
}

/// Merge every module's `env`/`aliases` over the profile's, and record which
/// LAYER each merged entry came from — a profile in the inheritance chain, a
/// subscribed source, or a module.
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
    layer_owners: &crate::config::EntryOwners,
    modules: &[ResolvedModule],
) -> (
    Vec<crate::config::EnvVar>,
    Vec<crate::config::ShellAlias>,
    EnvOrigins,
) {
    let mut merged = profile_env.to_vec();
    let mut merged_aliases = profile_aliases.to_vec();
    // Seeded from the layer merge that produced `profile_env`/`profile_aliases`,
    // so a profile-chain or source-delivered entry keeps the layer that
    // declared it; modules then claim over it, last writer winning exactly as
    // the value merge below does.
    let mut origins = EnvOrigins::from_owners(layer_owners);
    for module in modules {
        // A module's own entries were filtered when it resolved, so this fold
        // sees only what applies here; `PATH` concatenates onto the profile's
        // declaration rather than replacing it.
        crate::fold_env_layer(&mut merged, &module.env, crate::PATH_LIST_SEPARATOR);
        crate::merge_aliases(&mut merged_aliases, &module.aliases);
        origins.claim_module_entries(module);
    }
    (merged, merged_aliases, origins)
}

/// Re-derive the exact env targets the planner would write for this scope
/// and check each against what is actually on disk. Never touches the state
/// store — recording is the caller's, per scope (`verify`'s doc above). Every
/// non-matching row carries only the opaque `current`/`missing or changed`
/// markers: a declared value can be sensitive and a recorded row is read back
/// by `status`/`diff` AND shipped to the device gateway, so the real content
/// is recomputed from config at render time, never stored.
// NOTE: Secret-backed env vars (from SecretSpec.envs) are not included in
// verification because they require provider resolution. This means cfgd status
// may report env file drift after secret envs are written. This will be addressed
// when compliance snapshots track secret env metadata.
pub fn env_verify_results(
    profile_env: &[crate::config::EnvVar],
    profile_aliases: &[crate::config::ShellAlias],
    layer_owners: &crate::config::EntryOwners,
    scope: EnvScope,
    modules: &[ResolvedModule],
    path_dirs: &[ManagerPathDir],
) -> Vec<VerifyResult> {
    let mut results = Vec::new();
    let (merged, merged_aliases, origins) =
        merge_module_env_aliases(profile_env, profile_aliases, layer_owners, modules);

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
    let fold =
        super::env_engine::primary_folded_path(&merged, path_dirs, &origins, &home, platform);
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
                        fold.as_ref(),
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
                    unmanaged: false,
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

/// What a module-scoped env check answered: the per-item rows it could judge,
/// or the one probe failure that kept it from judging any.
pub struct EnvItemCheck {
    pub results: Vec<VerifyResult>,
    /// The primary env file exists but could not be read, so every item
    /// verdict is unknown — the same first-class "error checking drift" row an
    /// erroring system configurator mints, never a silent clean.
    pub check_error: Option<SystemCheckError>,
}

/// The per-item half of the env check ALONE, for a `--module`-scoped surface.
///
/// Each env var and alias the scope's merge declares is judged against the
/// line the primary managed env file holds — and nothing else is evaluated.
/// The whole-file staleness row, the rc source lines and the folded `PATH`
/// line are the whole profile's shared artifacts: a module isolate can
/// neither vouch for nor blame them ([`EntryOwners`](crate::config::EntryOwners)
/// records `PATH` with as many owners as contributed, because its
/// declarations concatenate across layers — which is why even a module that
/// declares `PATH` entries does not get a `PATH` row here).
///
/// A file that is NOT THERE answers per item: every declared line is
/// genuinely absent, and reporting nothing would read a deleted env surface
/// as converged. A file that exists but cannot be read answers nothing —
/// `check_error` says so instead.
pub fn env_item_verify_results(
    profile_env: &[crate::config::EnvVar],
    profile_aliases: &[crate::config::ShellAlias],
    layer_owners: &crate::config::EntryOwners,
    modules: &[ResolvedModule],
) -> EnvItemCheck {
    let (mut merged, merged_aliases, origins) =
        merge_module_env_aliases(profile_env, profile_aliases, layer_owners, modules);
    merged.retain(|ev| ev.name != "PATH");
    let mut check = EnvItemCheck {
        results: Vec::new(),
        check_error: None,
    };
    if merged.is_empty() && merged_aliases.is_empty() {
        return check;
    }
    let home = expand_tilde(std::path::Path::new("~"));
    let platform = EnvPlatform::current();
    let path = super::env_engine::primary_env_file_path(&home, platform);
    let actual = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            check.check_error = Some(SystemCheckError {
                key: to_posix_string(&path),
                error: e.to_string(),
            });
            return check;
        }
    };
    // `fold` is only ever consulted for a `PATH` row, and `PATH` was retained
    // out above.
    verify_env_items_in(
        &actual,
        &merged,
        &merged_aliases,
        &origins,
        platform,
        None,
        &mut check.results,
    );
    check
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
    fold: Option<&super::env_engine::FoldedPath>,
    results: &mut Vec<VerifyResult>,
) {
    let Ok(actual) = std::fs::read_to_string(path) else {
        return;
    };
    verify_env_items_in(&actual, env, aliases, origins, platform, fold, results);
}

/// The item loop of [`verify_env_items`] over content the caller already
/// read, so the scoped check below can decide for itself what an unreadable
/// file means instead of inheriting the silent early return the whole-file
/// check compensates for.
fn verify_env_items_in(
    actual: &str,
    env: &[crate::config::EnvVar],
    aliases: &[crate::config::ShellAlias],
    origins: &EnvOrigins,
    platform: EnvPlatform,
    fold: Option<&super::env_engine::FoldedPath>,
    results: &mut Vec<VerifyResult>,
) {
    let actual_lines: std::collections::HashSet<&str> = actual.lines().collect();

    for ev in env {
        let Some(line) = super::env_files::primary_env_var_line(ev, platform, origins, fold) else {
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
            // declared value (`export EDITOR="nvim"`), and the CLI recording
            // seam stores each result's operands verbatim in `drift_events`,
            // which flows on to the device gateway. A display surface that
            // wants the actual line recomputes it from the declared config at
            // render time — see `env_item_declared_line`.
            expected: "current".to_string(),
            actual: if matches {
                "current".to_string()
            } else {
                "missing or changed".to_string()
            },
            unmanaged: false,
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
            unmanaged: false,
        });
    }
}

/// The profile-plus-modules env/alias merge, resolved ONCE and then asked per
/// drift row. The merge clones the profile's env and aliases, clones the two
/// origin maps and folds every resolved module in, and a command rendering a
/// drift report asks about one row at a time — built per row, one report paid
/// for that merge once per finding. It is scoped to a command's own render and
/// never held: it is a reading of the declaration as it stands right now.
pub struct MergedEnvItems {
    env: Vec<crate::config::EnvVar>,
    aliases: Vec<crate::config::ShellAlias>,
    origins: EnvOrigins,
    path: Option<super::env_engine::FoldedPath>,
}

impl MergedEnvItems {
    /// Merge `env`/`aliases` (the profile's own) with every module's, exactly
    /// as the write and the verify pass do.
    ///
    /// `path_dirs` is the second producer of the file's one `PATH` line — the
    /// recorded bootstrapped-manager directories, the same slice
    /// `env_verify_results` is given. Without it a `PATH` row would be shown a
    /// line assembled from half the producers, which is not a line the file
    /// holds.
    pub fn new(
        env: &[crate::config::EnvVar],
        aliases: &[crate::config::ShellAlias],
        layer_owners: &crate::config::EntryOwners,
        modules: &[ResolvedModule],
        path_dirs: &[ManagerPathDir],
    ) -> Self {
        let (env, aliases, origins) = merge_module_env_aliases(env, aliases, layer_owners, modules);
        let path = super::env_engine::primary_folded_path(
            &env,
            path_dirs,
            &origins,
            &expand_tilde(std::path::Path::new("~")),
            EnvPlatform::current(),
        );
        Self {
            env,
            aliases,
            origins,
            path,
        }
    }

    /// The line a declared env var or alias renders as, for a DISPLAY surface
    /// that wants to show a drifted item's real value rather than the opaque
    /// `current`/`missing or changed` markers `verify_env_items` returns.
    /// Never called from a path that persists or ships its result: that is
    /// exactly the content the opaque markers exist to keep out of
    /// `drift_events` and the device gateway. `resource_type` is `"env-var"` or
    /// `"alias"`; any other kind (or an item no longer declared) answers `None`.
    ///
    /// The line shown is the line the file must hold — including the
    /// ` # module:<name>` comment a module-declared entry carries, which is
    /// part of what verify matched on. A module's entries are not in the
    /// profile's own `env`/`aliases` at all, so without the merge a
    /// module-owned row could only ever answer `None`.
    pub fn declared_line(&self, resource_type: &str, resource_id: &str) -> Option<String> {
        let platform = EnvPlatform::current();
        match resource_type {
            "env-var" => self
                .env
                .iter()
                .find(|e| e.name == resource_id)
                .and_then(|e| {
                    super::env_files::primary_env_var_line(
                        e,
                        platform,
                        &self.origins,
                        self.path.as_ref(),
                    )
                }),
            "alias" => self
                .aliases
                .iter()
                .find(|a| a.name == resource_id)
                .and_then(|a| super::env_files::primary_alias_line(a, platform, &self.origins)),
            _ => None,
        }
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

impl MergedEnvItems {
    /// The DISPLAY `(want, have)` pair for one env-var/alias row, recomputed
    /// from the machine: `want` is the line the current declaration renders as
    /// ([`Self::declared_line`]), `have` is the line the managed file actually
    /// holds (`deployed_env_item_line`), or [`crate::Absence::Missing`] when
    /// no deployed line claims the name. `None` for any other resource kind,
    /// for an item no longer declared, and for a managed file that exists but
    /// could not be read — the caller keeps the operands it already has. Being
    /// unable to LOOK is not the same fact as the entry being gone, and only
    /// the second may be reported as an absence.
    ///
    /// This is the one place that recompute happens, so `diff`, `verify`,
    /// `status` and `status --scan` cannot word the same env var four ways.
    /// Same "never call this on a value about to be persisted or shipped to the
    /// gateway" rule as [`Self::declared_line`]: both halves are real values,
    /// which is exactly what the opaque `current` / `missing or changed`
    /// markers exist to keep out of `drift_events`.
    pub fn display_values(
        &self,
        resource_type: &str,
        resource_id: &str,
    ) -> Option<(String, String)> {
        let declared = self.declared_line(resource_type, resource_id)?;
        let deployed = match deployed_env_item_line(resource_type, resource_id) {
            Ok(Some(line)) => line,
            Ok(None) => crate::Absence::Missing.as_str().to_string(),
            Err(_) => return None,
        };
        Some((declared, deployed))
    }
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
                unmanaged: false,
            });
        }
        Ok(_) => {
            results.push(VerifyResult {
                resource_type: "env".to_string(),
                resource_id: to_posix_string(path),
                matches: false,
                expected: "current".to_string(),
                actual: "stale".to_string(),
                unmanaged: false,
            });
        }
        Err(_) => {
            results.push(VerifyResult {
                resource_type: "env".to_string(),
                resource_id: to_posix_string(path),
                matches: false,
                expected: "present".to_string(),
                actual: "missing".to_string(),
                unmanaged: false,
            });
        }
    }
}
