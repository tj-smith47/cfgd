//! Deciding what to do about a target cfgd is planning to write but did not
//! put there.
//!
//! Two halves live here: the CLASSIFICATION (is this file cfgd's or the
//! user's?) and the SWEEP that settles every conflicting target in a plan with
//! an already-decided policy. Both belong to the reconciler rather than to the
//! CLI, because the daemon reconciles the same plans against the same machine
//! and had no access to either — an auto-apply displaced a user's file with no
//! copy kept, while `cfgd apply` over the identical plan copied it aside. The
//! CLI keeps only the part core cannot have: the interactive prompt.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::PathDisplayExt;
use crate::config::FileStrategy;
use crate::errors::FileError;
use crate::output::{Printer, Role, Spinner};
use crate::providers::FileAction;
use crate::state::StateStore;

use super::{Action, ModuleActionKind, Owner, Plan};

/// The reason a target skipped for holding an unmanaged file reports, shared by
/// the profile action's `Skip` reason and the module arm's status line so the
/// two cannot describe the same decision differently.
pub const UNMANAGED_SKIP_REASON: &str = "skipped: target exists as unmanaged file";

/// A conflict policy that has been SETTLED for one target.
///
/// The CLI's `--on-conflict ask` is a request, never an outcome: by the time a
/// target is acted on, the question has been answered — by the prompt, by
/// `--yes`, or by there being nobody to ask. Giving the answer its own type
/// without an `Ask` variant is what makes "ask" unrepresentable at the
/// executors, where it had been folded into their `Overwrite` catch-all — so a
/// run that asked to be asked, and could not be, destroyed the file instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolvedConflict {
    /// Copy the existing file aside, then write.
    Backup,
    /// Replace the existing file, keeping no copy of it.
    Overwrite,
    /// Leave the existing file alone and drop cfgd's write.
    Skip,
    /// Abort the apply without touching the file.
    Fail,
}

/// Whether a file target is a file cfgd never wrote — it exists on disk and
/// nothing cfgd recorded accounts for it.
///
/// The state store is asked TWO questions, not one: whether a module deployed
/// this file (`module_file_manifest`, keyed by posix fs key) and only then
/// whether a profile `spec.files` target claims it (`managed_resources`).
/// Asking the second alone made a module-deployed file whose link a user
/// replaced read as a stranger's — the module deploy path records into the
/// manifest and writes no `managed_resources` row at all, so the file cfgd
/// itself put there was reserved for adoption.
///
/// Three targets are excluded before either lookup: one already holding the
/// bytes the write would put there (converged is not conflicted), a `Patch`
/// entry (it merges in place, so every conflict outcome is wrong for it), and a
/// symlink into the config dir or the module cache (cfgd's own).
pub fn is_unmanaged_file(target: &Path, config_dir: &Path, state: &StateStore) -> bool {
    if !target.exists() && target.symlink_metadata().is_err() {
        return false;
    }

    // A symlink into the config dir, or into the module cache, is cfgd's own
    // deployment however the state store reads.
    if let Ok(link_target) = target.read_link() {
        if link_target.starts_with(config_dir) {
            return false;
        }
        // Off the resolved cache root, never a `~/.cache` literal: the root is
        // `%LOCALAPPDATA%\cfgd` on Windows and `~/Library/Caches/cfgd` on
        // macOS, so a hand-spelled POSIX path answers about a directory that
        // exists on Linux alone and cfgd's own deployment reads as a
        // stranger's everywhere else. An unresolvable cache root leaves the
        // question unanswered here, exactly as an unreadable link does.
        if let Ok(module_cache) = crate::modules::default_module_cache_dir()
            && link_target.starts_with(&module_cache)
        {
            return false;
        }
    }

    // A module deploys its files under ONE aggregate `module` resource row, so
    // its per-file record lives in the module file manifest and nowhere else.
    // Asking only about `file` rows read every module-deployed target as a
    // stranger's the moment its symlink was replaced by a regular file. The
    // manifest key is `to_posix_fs_key`, so the lookup folds the same way.
    if let Ok(true) = state.is_module_deployed_file(&crate::to_posix_fs_key(target)) {
        return false;
    }

    // The id is minted posix-folded (`reconciler::format`), so the lookup folds
    // too: asked with native separators, every managed file on Windows answers
    // "unmanaged" and the conflict pass copies cfgd's OWN files aside on every
    // apply.
    let target_str = crate::to_posix_string(target);
    if let Ok(managed) = state.is_resource_managed("file", &target_str) {
        return !managed;
    }

    true
}

/// What a drift row says about a target holding a file cfgd never wrote.
///
/// `content differs from source` describes a file cfgd owns and lost track of,
/// which is a different problem with a different fix: this one is resolved by
/// deciding whether to adopt the user's file, not by re-writing it.
pub const UNMANAGED_DRIFT_CAUSE: &str = "unmanaged file at target";

/// Re-word a drifted file finding whose target cfgd never wrote, and record
/// that fact for a structured reader.
///
/// Applied to the finding rather than inside the detector because the detector
/// compares bytes and knows nothing of the state store; a converged finding is
/// left alone, and so is a target that is merely MISSING — absence is not a
/// stranger's file.
///
/// `strategy` is the entry's own, and a `Patch` one is left alone for the same
/// reason the sweep never prompts about it: the strategy adopts the target's
/// bytes in place, so its target is nobody's conflict. The parameter is what
/// makes that structural — the exclusion lived at the three call sites, and the
/// one that could not reach a strategy overwrote a patch failure's own reason
/// with this cause.
pub fn mark_unmanaged_drift(
    record: &mut crate::providers::FileDriftResult,
    strategy: FileStrategy,
    config_dir: &Path,
    state: &StateStore,
) {
    // Only a CONTENT comparison may be re-worded. A finding whose desired
    // content could not be determined at all (`source not found: …`) says why
    // cfgd could not look, and replacing that with "unmanaged file at target"
    // answers a question nobody asked while losing the only actionable half.
    // Judged on `expected` because the type carries strings and nothing else —
    // the literal is shared with its producer so the two cannot drift.
    if record.matches
        || adopts_in_place(strategy)
        || record.expected == crate::providers::SOURCE_MISSING_EXPECTED
    {
        return;
    }
    if is_unmanaged_file(Path::new(&record.target), config_dir, state) {
        record.unmanaged = true;
        record.actual = UNMANAGED_DRIFT_CAUSE.to_string();
    }
}

/// Whether a strategy adopts an existing unmanaged target in place instead of
/// replacing it.
///
/// `Patch` merges into the target's own bytes, so the unmanaged-file prompt
/// must never fire for it: every one of its choices is wrong. "Adopt
/// (overwrite)" misdescribes a merge, and "Backup" renames the target away
/// *before* apply — the merge would then read an empty current content and
/// write only the ensured keys, destroying exactly the content the strategy
/// exists to preserve.
fn adopts_in_place(strategy: FileStrategy) -> bool {
    matches!(strategy, FileStrategy::Patch)
}

/// Whether `target` already holds exactly the bytes the planned write would
/// put there.
///
/// The adoption short-circuit: a converged target is not a conflict, so it must
/// not be prompted about, copied aside, or rewritten. `desired_hash` is `None`
/// whenever the content is not knowable ahead of the write — a link strategy, a
/// `Patch` merge, an unreadable source — and that answers "not converged", so
/// the conflict path runs exactly as before.
///
/// Judged on `symlink_metadata`, never `exists()`: a symlink at the target is a
/// thing to replace, not content to compare, however its destination reads.
fn target_holds_desired_content(target: &Path, desired_hash: Option<&str>) -> bool {
    let Some(want) = desired_hash else {
        return false;
    };
    let Ok(meta) = target.symlink_metadata() else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    match std::fs::read(target) {
        Ok(bytes) => crate::sha256_hex(&bytes) == want,
        Err(_) => false,
    }
}

/// The hash of the bytes a module file deployment will write, when that is
/// answerable before the deployment runs.
///
/// Only a `Copy`/`Template` entry writes whole content (both read the source
/// verbatim in `reconciler::modules`); a link entry replaces the target with a
/// link and a `Patch` entry merges into whatever the target already holds, so
/// neither has a comparable "desired content" at all.
///
/// `strategy` is the RESOLVED strategy, matching what `reconciler::modules`
/// will act on: a file declaring none of its own under a global
/// `fileStrategy: copy` writes whole content just the same, and reading the
/// unresolved field would answer `None` and re-adopt it on every apply.
pub fn module_file_desired_hash(
    file: &crate::modules::ResolvedFile,
    strategy: FileStrategy,
) -> Option<String> {
    if !matches!(strategy, FileStrategy::Copy | FileStrategy::Template) {
        return None;
    }
    if !file.source.is_file() {
        return None;
    }
    std::fs::read(&file.source)
        .ok()
        .map(|bytes| crate::sha256_hex(&bytes))
}

/// The `--on-conflict fail` abort, worded the same for a profile file and a
/// module one.
pub fn unmanaged_conflict_error(target: &Path, module_name: Option<&str>) -> FileError {
    FileError::UnmanagedTarget {
        path: target.to_path_buf(),
        module: module_name.map(str::to_string),
    }
}

/// The Planning bar's label while the sweep reads one owner's targets. Named
/// so the test asserting a PROMPTING sweep opens no bar can look for the same
/// string the narrating one writes — a bare literal in that negative goes
/// vacuous the moment this is reworded.
pub fn sweep_label(owner: &Owner) -> String {
    format!("Checking existing files for {}", owner.token())
}

/// Answers what to do with one target holding a file cfgd never wrote, given
/// its path and the module that claims it (`None` for a profile file).
///
/// The one decision [`sweep_unmanaged_file_targets`] cannot make for itself:
/// the CLI's implementation prompts, the daemon's answers `Backup`, and a
/// settled `--on-conflict` answers the same policy every time.
pub type ConflictResolver<'a> =
    &'a mut dyn FnMut(&Path, Option<&str>) -> Result<ResolvedConflict, FileError>;

/// Settle every target that already holds a file cfgd never wrote, and report
/// the ones whose settled policy is `Backup`.
///
/// Nothing on disk is touched here. A `Backup` decision is carried out by the
/// action that displaces the target, inside the phase that runs it
/// ([`super::Reconciler::backing_up`]) — a plan is a preview until the operator
/// confirms it, and the line reporting a copy belongs beside the write it
/// protects rather than above the run's own header.
///
/// `resolve` answers the policy for one target; a caller with a settled policy
/// hands back the same answer every time, and the CLI's interactive caller
/// prompts. It is the whole of what core cannot decide.
///
/// Three callers sweep — `apply`, `init`'s apply, and the daemon's tick, the
/// last only when its drift policy is `Auto`, since a report-only tick
/// displaces nothing and any sidecar it took would copy a file nobody was about
/// to overwrite. `cfgd plan` deliberately never sweeps and must not start: a
/// preview that copied the user's files aside would mutate the disk to describe
/// what an apply WOULD do.
///
/// Nothing here announces a conflict ahead of the run header. The sweep is
/// silent, the prompt says its own piece, and a settled `Backup` reports itself
/// on the action row that performs the copy.
pub fn sweep_unmanaged_file_targets(
    plan: &mut Plan,
    config_dir: &Path,
    state: &StateStore,
    printer: &Printer,
    strategies: &crate::effective::FileStrategies,
    mut spinner: Option<&mut Spinner>,
    resolve: ConflictResolver<'_>,
) -> Result<HashSet<PathBuf>, FileError> {
    // Targets whose settled policy is `Backup`, handed to the reconciler so the
    // copy runs with the write it protects.
    let mut backups: HashSet<PathBuf> = HashSet::new();
    // Targets the pass decided to leave alone. Planning emits a `SetPermissions`
    // as a SIBLING of the write, so rewriting only the write leaves a chmod
    // behind — and "skip" would still change the mode of the file it promised
    // not to touch.
    let mut skipped: Vec<PathBuf> = Vec::new();

    for phase in &mut plan.phases {
        for (owner, actions) in phase.groups_mut() {
            if let Some(sp) = spinner.as_deref_mut() {
                sp.set_message(sweep_label(owner));
            }
            let mut i = 0;
            while i < actions.len() {
                // Profile file actions
                if let Action::File(
                    FileAction::Create {
                        target,
                        strategy,
                        source_hash,
                        ..
                    }
                    | FileAction::Update {
                        target,
                        strategy,
                        source_hash,
                        ..
                    },
                ) = &actions[i]
                {
                    let target = target.clone();
                    let strategy = *strategy;
                    let desired = source_hash.clone();
                    if !adopts_in_place(strategy)
                        && !target_holds_desired_content(&target, desired.as_deref())
                        && is_unmanaged_file(&target, config_dir, state)
                    {
                        let chosen = resolve(&target, None)?;
                        if chosen == ResolvedConflict::Skip {
                            skipped.push(target.clone());
                        }
                        apply_conflict_policy(chosen, &target, &mut actions[i], &mut backups)?;
                    }
                }

                // Module file actions
                if let Action::Module(ref mut ma) = actions[i]
                    && let ModuleActionKind::DeployFiles {
                        ref mut files,
                        ref mut declared_total,
                    } = ma.kind
                {
                    let module_name = ma.module_name.clone();
                    let mut j = 0;
                    while j < files.len() {
                        let file_target = crate::expand_tilde(&files[j].target);
                        let strategy = strategies.for_target(&file_target);
                        let desired = module_file_desired_hash(&files[j], strategy);
                        if !adopts_in_place(strategy)
                            && !target_holds_desired_content(&file_target, desired.as_deref())
                            && is_unmanaged_file(&file_target, config_dir, state)
                        {
                            match resolve(&file_target, Some(&module_name))? {
                                ResolvedConflict::Backup => {
                                    backups.insert(file_target.clone());
                                }
                                ResolvedConflict::Skip => {
                                    // A dropped module file leaves no action to
                                    // render, so the decision is reported here
                                    // or nowhere — the profile arm's `Skip`
                                    // action says the same thing in the tree.
                                    printer
                                        .status(
                                            Role::Skipped,
                                            format!(
                                                "module '{}': {}",
                                                module_name,
                                                file_target.posix()
                                            ),
                                        )
                                        .detail(UNMANAGED_SKIP_REASON);
                                    skipped.push(file_target);
                                    files.remove(j);
                                    // The skipped file leaves the declared set
                                    // with it: a `k of N files` detail must
                                    // mean the other N−k converged, and this
                                    // one was refused, not converged.
                                    *declared_total = declared_total.saturating_sub(1);
                                    continue;
                                }
                                ResolvedConflict::Fail => {
                                    return Err(unmanaged_conflict_error(
                                        &file_target,
                                        Some(&module_name),
                                    ));
                                }
                                ResolvedConflict::Overwrite => {}
                            }
                        }
                        j += 1;
                    }
                }

                i += 1;
            }
        }
    }

    prune_skipped_leftovers(plan, &skipped);
    Ok(backups)
}

/// Clear away what a skipped target leaves behind in the plan.
///
/// Two leftovers, both of which contradict what "skip" was told to mean:
///
/// - the sibling `SetPermissions` planning pairs with every `Create`/`Update`.
///   Left in place, `--on-conflict skip` still changes the mode of the file it
///   undertook to leave untouched — a smaller edit than the write, and the same
///   broken promise. Swept over the whole plan rather than the neighbouring
///   index, so a phase that groups the pair differently cannot reintroduce it
/// - a module deployment whose every file was skipped, which would otherwise
///   render and journal a deployment of nothing
fn prune_skipped_leftovers(plan: &mut Plan, skipped: &[PathBuf]) {
    for phase in &mut plan.phases {
        phase.retain_actions(|action| match action {
            Action::File(FileAction::SetPermissions { target, .. }) => {
                !skipped.iter().any(|s| s == target)
            }
            Action::Module(ma) => !matches!(
                &ma.kind,
                ModuleActionKind::DeployFiles { files, .. } if files.is_empty()
            ),
            _ => true,
        });
    }
    plan.phases.retain(|p| !p.is_empty());
}

/// Carry out one resolved policy against one profile-file action.
pub fn apply_conflict_policy(
    policy: ResolvedConflict,
    target: &Path,
    action: &mut Action,
    backups: &mut HashSet<PathBuf>,
) -> Result<(), FileError> {
    match policy {
        ResolvedConflict::Backup => {
            backups.insert(target.to_path_buf());
        }
        ResolvedConflict::Skip => {
            let origin = match action {
                Action::File(FileAction::Create { origin, .. })
                | Action::File(FileAction::Update { origin, .. }) => origin.clone(),
                _ => crate::config::LOCAL_LAYER.to_string(),
            };
            *action = Action::File(FileAction::Skip {
                target: target.to_path_buf(),
                reason: UNMANAGED_SKIP_REASON.to_string(),
                origin,
            });
        }
        ResolvedConflict::Fail => return Err(unmanaged_conflict_error(target, None)),
        ResolvedConflict::Overwrite => {}
    }
    Ok(())
}
