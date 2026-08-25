use std::fs;
use std::path::{Path, PathBuf};

use similar::TextDiff;

use cfgd_core::PathDisplayExt;
use cfgd_core::config::{
    EncryptionMode, FileStrategy, LOCAL_LAYER, ManagedFileSpec, MergedProfile, PatchSpec,
};
use cfgd_core::errors::{FileError, Result};
use cfgd_core::expand_tilde;
use cfgd_core::output::{Printer, Role};
use cfgd_core::providers::{FileAction, FileDriftResult};
use cfgd_core::reconciler::{
    PatchBinding, PatchOutcome, ReconcileContext, evaluate_patch, patch_failure_detail,
};

use super::template::is_tera_template;

/// Whether `target_path` is linked to `source_path` under `strategy` —
/// symlink identity for [`FileStrategy::Symlink`], same inode for
/// [`FileStrategy::Hardlink`], `false` for anything else (Copy/Template
/// convergence is a content question — see [`directories_content_equal`]).
///
/// The single home for this check: `plan()` used to branch on the same two
/// predicates directly, once per strategy, and the diff-side directory guard
/// duplicated the pair again to cover the case `plan()` never reaches (a
/// directory-shaped SOURCE — never true of a profile file, which
/// `scan_directory` expands one file at a time, but routine for a module
/// file: a whole `lua/`/`after/` tree deployed by symlink).
///
/// `is_same_inode` follows symlinks (`std::fs::metadata`), so on Unix a
/// RELATIVE directory symlink resolves to the source's inode and this arm
/// correctly reports it converged even though `read_link() == source_path`
/// just rejected the relative target string — it is not dead weight next to
/// the symlink check, it is what rescues that case (and bind mounts). On
/// Windows the inode arm is inert for a directory: opening one without
/// `FILE_FLAG_BACKUP_SEMANTICS` fails, so a relative directory symlink there
/// reports false drift and only an absolute link converges.
fn is_linked_to(source_path: &Path, target_path: &Path, strategy: FileStrategy) -> bool {
    match strategy {
        FileStrategy::Symlink => target_path
            .read_link()
            .map(|link| link == source_path)
            .unwrap_or(false),
        FileStrategy::Hardlink => cfgd_core::is_same_inode(source_path, target_path),
        _ => false,
    }
}

/// Describe why a directory-shaped target is not linked to its source
/// (Symlink/Hardlink strategy), for the `actual` field of a drifted
/// [`FileDriftResult`] (and the matching status line `diff_one` prints).
/// Never reads either side's content.
fn describe_unlinked(target_path: &Path) -> String {
    if target_path.symlink_metadata().is_err() {
        "missing".to_string()
    } else if target_path.is_symlink() {
        "symlink points elsewhere".to_string()
    } else {
        "present but not linked to managed source".to_string()
    }
}

/// [`describe_unlinked`]'s counterpart for a Copy/Template directory: the
/// target is never expected to BE a link, so "not linked" would misdescribe
/// what actually differs.
fn describe_directory_unequal(target_path: &Path) -> String {
    if target_path.symlink_metadata().is_err() {
        "missing".to_string()
    } else if !target_path.is_dir() {
        "present but not a directory".to_string()
    } else {
        "directory content differs from source".to_string()
    }
}

/// Whether `target_dir` holds a byte-identical copy of every file under
/// `source_dir` — the Copy/Template counterpart to [`is_linked_to`], for a
/// directory-shaped managed entry deployed by `copy_dir_recursive` rather
/// than symlinked (the usual Windows choice when Developer Mode is off, and
/// any profile with `files.strategy: copy` globally). Recurses into
/// subdirectories; skips symlinks on the SOURCE side, mirroring
/// `copy_dir_recursive` itself, which never follows one out of the source
/// tree — a symlinked source entry is never something a copy claims to have
/// written, so it is not something convergence checks either. A target entry
/// with no counterpart in `source_dir` is NOT drift: a recursive copy never
/// removes what it did not put there, so this only requires that everything
/// `source_dir` names is present in `target_dir` with matching bytes, not
/// that the two trees are identically shaped.
///
/// Errors on the SOURCE side propagate — cfgd owns that tree, so an I/O
/// failure reading it is a real problem, not a convergence answer. A missing
/// or unreadable TARGET entry is reported as `false` (drift), matching every
/// other missing-target case in this file.
fn directories_content_equal(source_dir: &Path, target_dir: &Path) -> Result<bool> {
    for entry in fs::read_dir(source_dir).map_err(|e| FileError::Io {
        path: source_dir.to_path_buf(),
        source: e,
    })? {
        let entry = entry.map_err(|e| FileError::Io {
            path: source_dir.to_path_buf(),
            source: e,
        })?;
        let file_type = entry.file_type().map_err(|e| FileError::Io {
            path: entry.path(),
            source: e,
        })?;
        if file_type.is_symlink() {
            continue;
        }
        let target_entry = target_dir.join(entry.file_name());
        if file_type.is_dir() {
            if !target_entry.is_dir() {
                return Ok(false);
            }
            if !directories_content_equal(&entry.path(), &target_entry)? {
                return Ok(false);
            }
        } else {
            let source_bytes = fs::read(entry.path()).map_err(|e| FileError::Io {
                path: entry.path(),
                source: e,
            })?;
            let Ok(target_bytes) = fs::read(&target_entry) else {
                return Ok(false);
            };
            if source_bytes != target_bytes {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

impl super::CfgdFileManager {
    /// Resolve the effective strategy for a managed file.
    /// Template files always use Copy (can't symlink unrendered templates).
    pub(super) fn effective_strategy(
        &self,
        source: &Path,
        per_file: Option<FileStrategy>,
    ) -> FileStrategy {
        // `Patch` edits the target in place and never reads the source, so a
        // `.tera` source path must not silently downgrade it to a whole-file
        // Copy — that would overwrite exactly the content it promises to keep.
        if per_file == Some(FileStrategy::Patch) {
            return FileStrategy::Patch;
        }
        if is_tera_template(source) {
            return FileStrategy::Copy;
        }
        per_file.unwrap_or(self.global_strategy)
    }

    /// Script-execution binding for a profile-declared `Patch` file: a relative
    /// `patch.script` resolves against the config directory.
    fn patch_binding(&self, context: ReconcileContext) -> PatchBinding {
        PatchBinding::profile(&self.config_dir, &self.profile_name, context)
    }

    /// Current-vs-patched content for a profile-declared `Patch` file.
    ///
    /// The single evaluation entry point for the profile paths (`plan`, `diff`,
    /// `file_drift_results`) so all three agree on what "up to date" means.
    fn evaluate(
        &self,
        managed: &ManagedFileSpec,
        target: &Path,
        context: ReconcileContext,
    ) -> Result<PatchOutcome> {
        self.evaluate_spec(Self::patch_spec(managed, target)?, target, context)
    }

    /// [`Self::evaluate`] for callers that already hold the `patch` block —
    /// the apply path (which reads it off the planned action) and the dry-run
    /// preview.
    pub(crate) fn evaluate_spec(
        &self,
        spec: &PatchSpec,
        target: &Path,
        context: ReconcileContext,
    ) -> Result<PatchOutcome> {
        evaluate_patch(spec, target, &self.patch_binding(context).context())
    }

    /// The `patch` block a `Patch` entry must carry (guaranteed by
    /// `validate_managed_file_specs`; re-checked here because this module is
    /// reachable from callers that build specs programmatically).
    fn patch_spec<'s>(managed: &'s ManagedFileSpec, target: &Path) -> Result<&'s PatchSpec> {
        managed.patch.as_ref().ok_or_else(|| {
            FileError::PatchBlockMissing {
                path: target.to_path_buf(),
            }
            .into()
        })
    }

    /// Build a plan of file actions by comparing desired state (from profile) to actual state (on disk).
    pub fn plan(&self, profile: &MergedProfile) -> Result<Vec<FileAction>> {
        let mut actions = Vec::new();

        for managed in &profile.files.managed {
            let target_path = expand_tilde(&managed.target);

            // Branch on the declared strategy before resolving `source` at all:
            // a `Patch` entry has no source (an empty one would resolve to the
            // config directory itself, which `resolve_source_path` would hand
            // back as an "existing" path and the content comparison below would
            // then try to read as a file).
            if managed.strategy == Some(FileStrategy::Patch) {
                let origin = managed
                    .origin
                    .clone()
                    .unwrap_or_else(|| LOCAL_LAYER.to_string());
                let outcome = self.evaluate(managed, &target_path, ReconcileContext::Apply)?;
                // A converged target needs no write, but its declared mode can
                // still drift, so the permission check below runs either way.
                if !outcome.is_up_to_date() {
                    actions.push(if target_path.exists() {
                        FileAction::Update {
                            source: PathBuf::new(),
                            target: target_path.clone(),
                            diff: patch_unified_diff(&target_path, &outcome),
                            origin,
                            strategy: FileStrategy::Patch,
                            // Apply re-evaluates the merge against whatever the
                            // target holds at write time, so there is no planned
                            // content to invalidate: an out-of-band edit between
                            // plan and apply is folded in, not overwritten.
                            source_hash: None,
                            patch: managed.patch.clone(),
                        }
                    } else {
                        FileAction::Create {
                            source: PathBuf::new(),
                            target: target_path.clone(),
                            origin,
                            strategy: FileStrategy::Patch,
                            source_hash: None,
                            patch: managed.patch.clone(),
                        }
                    });
                }
                if let Some(action) = self.check_permissions(&target_path, managed, profile)? {
                    actions.push(action);
                }
                continue;
            }

            let source_path = self.resolve_source_path(&managed.source)?;

            if !source_path.exists() {
                if managed.private {
                    // Private files are local-only — skip silently on other machines
                    actions.push(FileAction::Skip {
                        target: target_path,
                        reason: "private (local only)".to_string(),
                        origin: managed
                            .origin
                            .clone()
                            .unwrap_or_else(|| LOCAL_LAYER.to_string()),
                    });
                    continue;
                }
                return Err(FileError::SourceNotFound { path: source_path }.into());
            }

            let strategy = self.effective_strategy(&source_path, managed.strategy);

            // Validate encryption requirements before planning.
            if let Some(enc) = &managed.encryption {
                // Always mode is incompatible with Symlink/Hardlink strategies because
                // those strategies expose the unencrypted source file on disk directly.
                if enc.mode == EncryptionMode::Always
                    && matches!(strategy, FileStrategy::Symlink | FileStrategy::Hardlink)
                {
                    return Err(FileError::EncryptionStrategyIncompatible {
                        path: source_path.clone(),
                        strategy: format!("{:?}", strategy),
                    }
                    .into());
                }

                // For InRepo (and Always) mode: the source file in the repo must be
                // encrypted with the declared backend.
                let encrypted = cfgd_core::is_file_encrypted(&source_path, &enc.backend)?;
                if !encrypted {
                    return Err(FileError::NotEncrypted {
                        path: source_path.clone(),
                        backend: enc.backend.clone(),
                    }
                    .into());
                }
            }
            let origin = managed
                .origin
                .clone()
                .unwrap_or_else(|| LOCAL_LAYER.to_string());

            // For symlink/hardlink: check if the target is already the correct link
            if matches!(strategy, FileStrategy::Symlink | FileStrategy::Hardlink) {
                let is_current = is_linked_to(&source_path, &target_path, strategy);

                if is_current {
                    if let Some(action) = self.check_permissions(&target_path, managed, profile)? {
                        actions.push(action);
                    }
                } else if target_path.exists() || target_path.symlink_metadata().is_ok() {
                    actions.push(FileAction::Update {
                        source: source_path.clone(),
                        target: target_path.clone(),
                        diff: format!("target will be re-linked ({:?})", strategy),
                        origin,
                        strategy,
                        source_hash: None,
                        patch: None,
                    });
                } else {
                    actions.push(FileAction::Create {
                        source: source_path.clone(),
                        target: target_path.clone(),
                        origin,
                        strategy,
                        source_hash: None,
                        patch: None,
                    });
                    if let Some(action) = self.check_permissions(&target_path, managed, profile)? {
                        actions.push(action);
                    }
                }
                continue;
            }

            // Copy/Template strategy: compare rendered content
            let rendered_content = if is_tera_template(&source_path) {
                self.render_template(&source_path, managed.origin.as_deref())?
            } else {
                fs::read_to_string(&source_path).map_err(|e| FileError::Io {
                    path: source_path.clone(),
                    source: e,
                })?
            };

            if target_path.exists() {
                let target_content =
                    fs::read_to_string(&target_path).map_err(|e| FileError::Io {
                        path: target_path.clone(),
                        source: e,
                    })?;

                if rendered_content == target_content {
                    if let Some(action) = self.check_permissions(&target_path, managed, profile)? {
                        actions.push(action);
                    }
                } else {
                    let diff = TextDiff::from_lines(&target_content, &rendered_content);
                    let unified = diff
                        .unified_diff()
                        .header(&target_path.display_posix(), &source_path.display_posix())
                        .to_string();

                    let content_hash = cfgd_core::sha256_hex(rendered_content.as_bytes());
                    actions.push(FileAction::Update {
                        source: source_path.clone(),
                        target: target_path.clone(),
                        diff: unified,
                        origin,
                        strategy,
                        source_hash: Some(content_hash),
                        patch: None,
                    });

                    if let Some(action) = self.check_permissions(&target_path, managed, profile)? {
                        actions.push(action);
                    }
                }
            } else {
                let content_hash = cfgd_core::sha256_hex(rendered_content.as_bytes());
                actions.push(FileAction::Create {
                    source: source_path.clone(),
                    target: target_path.clone(),
                    origin,
                    strategy,
                    source_hash: Some(content_hash),
                    patch: None,
                });

                if let Some(action) = self.check_permissions(&target_path, managed, profile)? {
                    actions.push(action);
                }
            }
        }

        Ok(actions)
    }

    /// Show diffs for all managed files, with syntax highlighting.
    /// The `(target, content hash)` pair of every managed file this profile
    /// deploys by Symlink/Hardlink whose target is converged on disk — the
    /// backing implementation of
    /// [`FileManager::link_deployed_content_hashes`](cfgd_core::providers::FileManager::link_deployed_content_hashes).
    ///
    /// Only single-file sources are reported. A directory-shaped entry (a
    /// module's whole `lua/` tree) has no one content hash to record, and the
    /// consumer asks a per-file question, so it is left out rather than answered
    /// with a digest of a tree.
    pub(super) fn link_deployed_content(
        &self,
        profile: &MergedProfile,
    ) -> Result<Vec<(PathBuf, String)>> {
        let mut deployed = Vec::new();

        for managed in &profile.files.managed {
            if managed.strategy == Some(FileStrategy::Patch) {
                continue;
            }
            let source_path = self.resolve_source_path(&managed.source)?;
            if !source_path.is_file() {
                continue;
            }
            let strategy = self.effective_strategy(&source_path, managed.strategy);
            if !matches!(strategy, FileStrategy::Symlink | FileStrategy::Hardlink) {
                continue;
            }
            let target_path = expand_tilde(&managed.target);
            if !is_linked_to(&source_path, &target_path, strategy) {
                continue;
            }
            // A source that exists but cannot be read leaves the question
            // unanswered, which is not the same as an answer of "unchanged":
            // report nothing for it and let the recorded hash stand.
            match fs::read(&source_path) {
                Ok(bytes) => deployed.push((target_path, cfgd_core::sha256_hex(&bytes))),
                Err(e) => {
                    tracing::debug!("cannot hash {}: {}", source_path.posix(), e);
                }
            }
        }

        Ok(deployed)
    }

    /// Render and print file diffs for the profile, returning one
    /// [`FileDriftResult`] per managed file. The caller reports drift when any
    /// record does not match, and serializes the records on the structured
    /// path so `-o json` carries the same per-file detail the terminal shows.
    pub fn diff(&self, profile: &MergedProfile, printer: &Printer) -> Result<Vec<FileDriftResult>> {
        let mut results = Vec::new();

        // Declaration order says nothing about the machine: two profiles that
        // deploy the same files report the same drift, in target order, so a
        // reader compares two runs rather than two spellings of one config.
        let mut managed_files: Vec<&_> = profile.files.managed.iter().collect();
        managed_files.sort_by(|a, b| a.target.cmp(&b.target));

        for managed in managed_files {
            if managed.strategy == Some(FileStrategy::Patch) {
                let target_path = expand_tilde(&managed.target);
                let evaluated = self.evaluate(managed, &target_path, ReconcileContext::Reconcile);
                results.push(render_patch_diff(&target_path, evaluated, printer));
                continue;
            }

            let source_path = self.resolve_source_path(&managed.source)?;
            results.push(self.diff_one(
                &source_path,
                &managed.target,
                managed.origin.as_deref(),
                managed.strategy,
                printer,
            )?);
        }

        Ok(results)
    }

    /// Render the inline content diff for a single source/target pair and
    /// return its drift record. `source_path` must already be resolved; `target` is
    /// `~`-expanded internally. A drifted target shows a unified diff; a missing
    /// target shows the would-be-created content syntax-highlighted; a missing
    /// source emits a warning and reports a non-matching record naming it. The
    /// record shape matches `Self::file_drift_one`. Shared by the profile-file path
    /// ([`Self::diff`]) and the module-file path so both render identically.
    ///
    /// `per_file_strategy` is the entry's own strategy override, if any — it is
    /// resolved to an effective [`FileStrategy`] internally via
    /// `Self::effective_strategy`, the same resolution `plan()` performs, so
    /// this and the plan agree on what "converged" means for the same entry.
    pub fn diff_one(
        &self,
        source_path: &Path,
        target: &Path,
        origin: Option<&str>,
        per_file_strategy: Option<FileStrategy>,
        printer: &Printer,
    ) -> Result<FileDriftResult> {
        let target_path = expand_tilde(target);
        let target_id = target_path.display_posix();

        if !source_path.exists() {
            printer.status_simple(
                Role::Warn,
                format!("Source not found: {}", source_path.posix()),
            );
            // Reported as a non-match, not as "no drift": the desired content
            // could not be determined, which is never the same as convergence.
            return Ok(FileDriftResult {
                target: target_id,
                matches: false,
                expected: cfgd_core::providers::SOURCE_MISSING_EXPECTED.to_string(),
                actual: format!("source not found: {}", source_path.posix()),
                unmanaged: false,
            });
        }

        // A directory-shaped managed entry (a module's whole `lua/` tree
        // deployed by symlink OR copy) has no single-file content to render —
        // the `fs::read_to_string` calls below error "Is a directory" the
        // instant either side is one. Content equality is also the wrong
        // question for a Symlink/Hardlink strategy: convergence there is link
        // identity. For Copy/Template it genuinely IS a content question, just
        // over every file in the tree rather than one — `directories_content_equal`
        // answers that. Branching on the entry's OWN resolved strategy (not a
        // blanket link-identity check) is what keeps a `files.strategy: copy`
        // profile — the usual Windows choice when Developer Mode is off — from
        // reporting a converged directory permanently drifted.
        if source_path.is_dir() || target_path.is_dir() {
            let strategy = self.effective_strategy(source_path, per_file_strategy);
            let uses_link_identity =
                matches!(strategy, FileStrategy::Symlink | FileStrategy::Hardlink);
            let matches = if uses_link_identity {
                is_linked_to(source_path, &target_path, strategy)
            } else {
                directories_content_equal(source_path, &target_path)?
            };
            let expected = if uses_link_identity {
                "linked to managed source"
            } else {
                "directory content matches source"
            };
            let actual = if matches {
                expected.to_string()
            } else if uses_link_identity {
                describe_unlinked(&target_path)
            } else {
                describe_directory_unequal(&target_path)
            };
            if !matches {
                printer.status_simple(Role::Info, format!("{} ({})", target_path.posix(), actual));
            }
            return Ok(FileDriftResult {
                target: target_id,
                matches,
                expected: expected.to_string(),
                actual,
                unmanaged: false,
            });
        }

        let rendered_content = if is_tera_template(source_path) {
            self.render_template(source_path, origin)?
        } else {
            fs::read_to_string(source_path).map_err(|e| FileError::Io {
                path: source_path.to_path_buf(),
                source: e,
            })?
        };

        if target_path.exists() {
            let target_content = fs::read_to_string(&target_path).map_err(|e| FileError::Io {
                path: target_path.clone(),
                source: e,
            })?;

            let matches = rendered_content == target_content;
            if !matches {
                printer.status_simple(Role::Info, target_path.display_posix());
                printer.diff(&target_content, &rendered_content);
            }
            Ok(FileDriftResult {
                target: target_id,
                matches,
                expected: "content matches source".to_string(),
                actual: if matches {
                    "content matches source".to_string()
                } else {
                    "content differs from source".to_string()
                },
                unmanaged: false,
            })
        } else {
            printer.status_simple(Role::Info, format!("{} (new file)", target_path.posix()));
            let lang = detect_language(&target_path);
            printer.syntax_highlight(&rendered_content, &lang);
            Ok(FileDriftResult {
                target: target_id,
                matches: false,
                expected: "present".to_string(),
                actual: "missing".to_string(),
                unmanaged: false,
            })
        }
    }

    /// Compute per-file content-drift results without rendering anything.
    ///
    /// Each managed file yields one [`FileDriftResult`] describing whether the
    /// on-disk target matches the rendered source content (presence AND bytes).
    /// This is the non-printing counterpart to [`Self::diff`]; it reuses the same
    /// source-render and target-compare logic so `verify` and the `status`/`verify`
    /// `--exit-code` gate share one content-aware detector instead of a presence-only
    /// check. A source that cannot be found is reported as a non-matching result
    /// rather than an error so a single bad entry can't mask drift elsewhere.
    pub(crate) fn file_drift_results(
        &self,
        profile: &MergedProfile,
    ) -> Result<Vec<FileDriftResult>> {
        let mut results = Vec::new();

        for managed in &profile.files.managed {
            if managed.strategy == Some(FileStrategy::Patch) {
                let target_path = expand_tilde(&managed.target);
                let evaluated = self.evaluate(managed, &target_path, ReconcileContext::Reconcile);
                results.push(patch_drift_result(&target_path, evaluated));
                continue;
            }

            results.push(self.file_drift_one(
                &self.resolve_source_path(&managed.source)?,
                &managed.target,
                managed.origin.as_deref(),
                managed.strategy,
            )?);
        }

        Ok(results)
    }

    /// Content-drift outcome for a single source/target pair.
    ///
    /// `source_path` must already be resolved (relative entries are resolved by
    /// the caller via [`Self::resolve_source_path`]); `target` is expanded for
    /// `~` internally. The source is rendered (tera template when the extension
    /// matches `origin`'s context, otherwise read as-is) and byte-compared to the
    /// on-disk target, yielding present/missing/differs. A source that cannot be
    /// found is reported as a non-matching result rather than an error so a single
    /// bad entry can't mask drift elsewhere. Shared by both the profile-file path
    /// ([`Self::file_drift_results`]) and the module-file path so every managed
    /// file is content-aware, not presence-only.
    ///
    /// `per_file_strategy` — see [`Self::diff_one`]'s doc; same resolution, same
    /// reason.
    pub(crate) fn file_drift_one(
        &self,
        source_path: &Path,
        target: &Path,
        origin: Option<&str>,
        per_file_strategy: Option<FileStrategy>,
    ) -> Result<FileDriftResult> {
        let target_path = expand_tilde(target);
        let target_id = target_path.display_posix();

        if !source_path.exists() {
            return Ok(FileDriftResult {
                target: target_id,
                matches: false,
                expected: cfgd_core::providers::SOURCE_MISSING_EXPECTED.to_string(),
                actual: format!("source not found: {}", source_path.posix()),
                unmanaged: false,
            });
        }

        // Same directory guard as `diff_one` — see its comment. `verify`,
        // `status --scan` and compliance all resolve through this
        // function, so a module's directory-strategy files (a whole `lua/`
        // tree deployed by symlink OR copy) would otherwise crash every one of
        // them with "Is a directory" the instant either side is one.
        if source_path.is_dir() || target_path.is_dir() {
            let strategy = self.effective_strategy(source_path, per_file_strategy);
            let uses_link_identity =
                matches!(strategy, FileStrategy::Symlink | FileStrategy::Hardlink);
            let matches = if uses_link_identity {
                is_linked_to(source_path, &target_path, strategy)
            } else {
                directories_content_equal(source_path, &target_path)?
            };
            let expected = if uses_link_identity {
                "linked to managed source"
            } else {
                "directory content matches source"
            };
            let actual = if matches {
                expected.to_string()
            } else if uses_link_identity {
                describe_unlinked(&target_path)
            } else {
                describe_directory_unequal(&target_path)
            };
            return Ok(FileDriftResult {
                target: target_id,
                matches,
                expected: expected.to_string(),
                actual,
                unmanaged: false,
            });
        }

        let rendered_content = if is_tera_template(source_path) {
            self.render_template(source_path, origin)?
        } else {
            fs::read_to_string(source_path).map_err(|e| FileError::Io {
                path: source_path.to_path_buf(),
                source: e,
            })?
        };

        if target_path.exists() {
            let target_content = fs::read_to_string(&target_path).map_err(|e| FileError::Io {
                path: target_path.clone(),
                source: e,
            })?;
            let matches = rendered_content == target_content;
            Ok(FileDriftResult {
                target: target_id,
                matches,
                expected: "content matches source".to_string(),
                actual: if matches {
                    "content matches source".to_string()
                } else {
                    "content differs from source".to_string()
                },
                unmanaged: false,
            })
        } else {
            Ok(FileDriftResult {
                target: target_id,
                matches: false,
                expected: "present".to_string(),
                actual: "missing".to_string(),
                unmanaged: false,
            })
        }
    }

    /// Check if permissions need to be changed for a target file.
    pub(super) fn check_permissions(
        &self,
        target: &Path,
        managed: &ManagedFileSpec,
        profile: &MergedProfile,
    ) -> Result<Option<FileAction>> {
        let target_str = target.display().to_string();

        // Per-file permissions take priority (intended for managed files).
        // Global files.permissions map is a fallback (intended for unmanaged paths,
        // but can also be used for managed files by target or source path).
        let mode_str = managed
            .permissions
            .as_ref()
            .or_else(|| profile.files.permissions.get(&target_str))
            .or_else(|| profile.files.permissions.get(&managed.source));

        if let Some(mode_str) = mode_str {
            // On Windows, file permissions are not applicable (NTFS uses inherited ACLs).
            // Skip generating SetPermissions actions and warn once. `warn!`, not
            // `info!`: a declared `mode:` being ignored is something the author
            // has to act on, and the default filter shows `warn` and above.
            #[cfg(windows)]
            {
                use std::sync::Once;
                static WARN_ONCE: Once = Once::new();
                WARN_ONCE.call_once(|| {
                    tracing::warn!(
                        "file permissions are not applicable on Windows (NTFS uses inherited ACLs); \
                         permissions settings will be ignored"
                    );
                });
                let _ = mode_str;
                return Ok(None);
            }
            #[cfg(not(windows))]
            {
                let desired_mode = cfgd_core::parse_octal_mode(mode_str).map_err(|_| {
                    FileError::TemplateError {
                        path: target.to_path_buf(),
                        message: format!("invalid permission mode: {}", mode_str),
                    }
                })?;

                if target.exists() {
                    let metadata = fs::metadata(target).map_err(|e| FileError::Io {
                        path: target.to_path_buf(),
                        source: e,
                    })?;
                    let current_mode = cfgd_core::file_permissions_mode(&metadata);

                    if current_mode != Some(desired_mode) {
                        return Ok(Some(FileAction::SetPermissions {
                            target: target.to_path_buf(),
                            mode: desired_mode,
                            origin: LOCAL_LAYER.to_string(),
                        }));
                    }
                } else {
                    // Target doesn't exist yet (will be created); emit SetPermissions
                    // so that apply sets the correct mode after creating the file.
                    return Ok(Some(FileAction::SetPermissions {
                        target: target.to_path_buf(),
                        mode: desired_mode,
                        origin: LOCAL_LAYER.to_string(),
                    }));
                }
            }
        }

        Ok(None)
    }

    /// Resolve a source path relative to the config directory.
    pub(crate) fn resolve_source_path(
        &self,
        source: &str,
    ) -> std::result::Result<PathBuf, FileError> {
        // The shared resolver, so the row `cfgd decide` renders for a withheld
        // `files.*` item describes the same bytes this action would write.
        cfgd_core::resolve_managed_file_source(source, &self.config_dir).ok_or_else(|| {
            FileError::PathTraversal {
                path: self.config_dir.join(source),
                root: self.config_dir.clone(),
            }
        })
    }
}

/// Script-execution binding for a module-deployed `Patch` file: a relative
/// `patch.script` resolves against the *module's* directory, and the filter
/// sees the module's `CFGD_MODULE_*` metadata and declared env. Used by the
/// read-only paths (`diff`, `verify`, `status --scan`), hence
/// `CFGD_CONTEXT=reconcile`.
pub(crate) fn module_patch_binding(
    config_dir: &Path,
    resolved: &cfgd_core::config::ResolvedProfile,
    module: &cfgd_core::modules::ResolvedModule,
) -> PatchBinding {
    PatchBinding::module(
        config_dir,
        resolved.profile_name(),
        ReconcileContext::Reconcile,
        module,
    )
}

/// Render one `Patch` file's inline diff and report its drift record.
///
/// The counterpart of [`crate::files::CfgdFileManager::diff_one`] for targets cfgd only
/// partially owns: a converged target prints nothing, a drifted one shows
/// current → merged, and a target that does not exist yet shows the content the
/// merge would create. Shared by the profile-file and module-file diff paths.
pub(crate) fn render_patch_diff(
    target: &Path,
    evaluated: Result<PatchOutcome>,
    printer: &Printer,
) -> FileDriftResult {
    match &evaluated {
        Err(e) => printer.status_simple(
            Role::Warn,
            format!("{}: {}", target.display_posix(), patch_failure_detail(e)),
        ),
        Ok(outcome) if !outcome.is_up_to_date() => {
            if target.exists() {
                printer.status_simple(Role::Info, target.display_posix());
                printer.diff(&outcome.current, &outcome.patched);
            } else {
                printer.status_simple(Role::Info, format!("{} (new file)", target.posix()));
                printer.syntax_highlight(&outcome.patched, &detect_language(target));
            }
        }
        Ok(_) => {}
    }
    // One record shape for the rendered and the silent path, so `diff -o json`
    // and `verify -o json` cannot describe the same file differently.
    patch_drift_result(target, evaluated)
}

/// Drift outcome for one `Patch` file: converged when re-running the merge over
/// the target's current content would change nothing.
///
/// An evaluation failure (an unparseable target, a filter that exits non-zero)
/// is reported as drift rather than propagated: read-only surfaces scan every
/// resource, and one broken filter must not blind the operator to unrelated
/// results. Write paths keep propagating the error — nothing may be written on
/// a guess.
pub(crate) fn patch_drift_result(
    target: &Path,
    evaluated: Result<PatchOutcome>,
) -> FileDriftResult {
    let outcome = match evaluated {
        Ok(o) => o,
        Err(e) => {
            return FileDriftResult {
                target: target.display_posix(),
                matches: false,
                expected: "content satisfies patch spec".to_string(),
                actual: patch_failure_detail(&e),
                unmanaged: false,
            };
        }
    };
    let matches = outcome.is_up_to_date();
    FileDriftResult {
        target: target.display_posix(),
        matches,
        expected: "content satisfies patch spec".to_string(),
        actual: if matches {
            "content satisfies patch spec".to_string()
        } else if target.exists() {
            "content differs from patch spec".to_string()
        } else {
            "missing".to_string()
        },
        unmanaged: false,
    }
}

/// Unified diff of a `Patch` target's current content against what the merge
/// would produce. Both sides name the target — unlike every other strategy the
/// "before" and "after" are the same file, not a source and a target.
pub(crate) fn patch_unified_diff(target: &Path, outcome: &PatchOutcome) -> String {
    let label = target.display_posix();
    TextDiff::from_lines(&outcome.current, &outcome.patched)
        .unified_diff()
        .header(&label, &label)
        .to_string()
}

/// Detect language from file extension for syntax highlighting.
pub(super) fn detect_language(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("txt")
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use cfgd_core::config::{
        EncryptionMode, EncryptionSpec, FileStrategy, FilesSpec, LayerPolicy, ManagedFileSpec,
        MergedProfile, ProfileLayer, ProfileSpec, ResolvedProfile,
    };
    use cfgd_core::output::{Printer, Verbosity};
    use cfgd_core::providers::{FileAction, FileManager};

    use super::super::CfgdFileManager;
    use super::detect_language;

    fn make_manager(config_dir: &std::path::Path) -> CfgdFileManager {
        let resolved = make_resolved(FilesSpec::default());
        CfgdFileManager::new(config_dir, &resolved).unwrap()
    }

    fn make_resolved(files: FilesSpec) -> ResolvedProfile {
        ResolvedProfile {
            layers: vec![ProfileLayer {
                source: "local".to_string(),
                profile_name: "test".to_string(),
                priority: 1000,
                policy: LayerPolicy::Local,
                spec: ProfileSpec::default(),
            }],
            merged: MergedProfile {
                files,
                ..Default::default()
            },
        }
    }

    fn spec(
        source: &str,
        target: std::path::PathBuf,
        strategy: Option<FileStrategy>,
    ) -> ManagedFileSpec {
        ManagedFileSpec {
            patch: None,
            source: source.to_string(),
            target,
            strategy,
            private: false,
            origin: None,
            encryption: None,
            permissions: None,
        }
    }

    #[test]
    fn content_drift_trait_delegates_matching() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();
        let source = config_dir.join("src.txt");
        let target = config_dir.join("target.txt");
        fs::write(&source, "hello world").unwrap();
        fs::write(&target, "hello world").unwrap();

        let fm = make_manager(config_dir);
        let result = FileManager::content_drift(&fm, &source, &target, None, None).unwrap();
        assert!(result.matches);
        assert_eq!(result.actual, "content matches source");
    }

    #[test]
    fn content_drift_trait_delegates_tampered() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();
        let source = config_dir.join("src.txt");
        let target = config_dir.join("target.txt");
        fs::write(&source, "hello world").unwrap();
        fs::write(&target, "tampered").unwrap();

        let fm = make_manager(config_dir);
        let result = FileManager::content_drift(&fm, &source, &target, None, None).unwrap();
        assert!(!result.matches);
        assert!(
            result.actual.contains("differs"),
            "expected 'differs' in actual, got: {}",
            result.actual
        );
    }

    #[test]
    fn content_drift_trait_delegates_missing_target() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();
        let source = config_dir.join("src.txt");
        let target = config_dir.join("absent.txt");
        fs::write(&source, "hello world").unwrap();

        let fm = make_manager(config_dir);
        let result = FileManager::content_drift(&fm, &source, &target, None, None).unwrap();
        assert!(!result.matches);
        assert_eq!(result.actual, "missing");
    }

    #[test]
    fn content_drift_trait_symlinked_directory_reports_no_drift() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();
        let source = config_dir.join("src_dir");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("a.lua"), "return {}").unwrap();
        let target = config_dir.join("target_dir");
        cfgd_core::create_symlink(&source, &target).unwrap();

        let fm = make_manager(config_dir);
        let result = FileManager::content_drift(&fm, &source, &target, None, None).unwrap();
        assert!(
            result.matches,
            "a directory correctly symlinked to its source must report no drift, got: {result:?}"
        );
    }

    #[test]
    fn content_drift_trait_directory_target_not_a_symlink_reports_drift_without_crashing() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();
        let source = config_dir.join("src_dir");
        fs::create_dir(&source).unwrap();
        let target = config_dir.join("target_dir");
        // A real directory, not a symlink — e.g. a Copy-strategy deployment,
        // or a symlink someone replaced by hand. `fs::read_to_string` on
        // either side of this pair is what used to error "Is a directory".
        fs::create_dir(&target).unwrap();

        let fm = make_manager(config_dir);
        let result = FileManager::content_drift(&fm, &source, &target, None, None).unwrap();
        assert!(!result.matches);
        assert_eq!(result.actual, "present but not linked to managed source");
    }

    #[test]
    fn content_drift_trait_directory_source_missing_target_reports_missing() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();
        let source = config_dir.join("src_dir");
        fs::create_dir(&source).unwrap();
        let target = config_dir.join("absent_dir");

        let fm = make_manager(config_dir);
        let result = FileManager::content_drift(&fm, &source, &target, None, None).unwrap();
        assert!(!result.matches);
        assert_eq!(result.actual, "missing");
    }

    #[test]
    fn diff_one_symlinked_directory_reports_no_drift_and_does_not_crash() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();
        let source = config_dir.join("src_dir");
        fs::create_dir(&source).unwrap();
        let target = config_dir.join("target_dir");
        cfgd_core::create_symlink(&source, &target).unwrap();

        let fm = make_manager(config_dir);
        let printer = Printer::for_test().0;
        let result = fm.diff_one(&source, &target, None, None, &printer).unwrap();
        assert!(result.matches);
    }

    #[test]
    fn diff_one_directory_replaced_by_plain_directory_reports_drift_without_crashing() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();
        let source = config_dir.join("src_dir");
        fs::create_dir(&source).unwrap();
        let target = config_dir.join("target_dir");
        fs::create_dir(&target).unwrap();

        let fm = make_manager(config_dir);
        let printer = Printer::for_test().0;
        // The regression this guards: before the directory guard, this call
        // returned `Err(FileError::Io { .. Is a directory .. })` instead of a
        // drift record.
        let result = fm.diff_one(&source, &target, None, None, &printer).unwrap();
        assert!(!result.matches);
    }

    /// A `strategy: copy` directory deployment — the usual Windows choice
    /// when Developer Mode is off — has no symlink and no shared inode by
    /// design, so a converged one must NOT report the permanent false drift
    /// a link-identity-only guard produced.
    #[test]
    fn diff_one_copy_deployed_directory_reports_no_drift_when_converged() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();
        let source = config_dir.join("src_dir");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("a.lua"), "return {}").unwrap();
        fs::create_dir(source.join("sub")).unwrap();
        fs::write(source.join("sub").join("b.lua"), "return 1").unwrap();
        let target = config_dir.join("target_dir");
        // The real deployment path for a Copy-strategy directory (see
        // `reconciler/modules.rs`'s `_ => copy_dir_recursive(...)` arm), not a
        // hand-rolled stand-in.
        cfgd_core::copy_dir_recursive(&source, &target).unwrap();

        let fm = make_manager(config_dir);
        let printer = Printer::for_test().0;
        let result = fm
            .diff_one(&source, &target, None, Some(FileStrategy::Copy), &printer)
            .unwrap();
        assert!(
            result.matches,
            "a converged copy-deployed directory must report no drift, got: {result:?}"
        );
        assert_eq!(result.actual, "directory content matches source");
    }

    /// A file tampered INSIDE a copy-deployed directory must still be
    /// caught — the drift-detection fix above must not trade a false
    /// positive for a false negative.
    #[test]
    fn diff_one_copy_deployed_directory_reports_drift_when_a_file_inside_is_tampered() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();
        let source = config_dir.join("src_dir");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("a.lua"), "return {}").unwrap();
        let target = config_dir.join("target_dir");
        cfgd_core::copy_dir_recursive(&source, &target).unwrap();
        fs::write(target.join("a.lua"), "# not mine").unwrap();

        let fm = make_manager(config_dir);
        let printer = Printer::for_test().0;
        let result = fm
            .diff_one(&source, &target, None, Some(FileStrategy::Copy), &printer)
            .unwrap();
        assert!(
            !result.matches,
            "a tampered file inside the deployed directory must be reported as drift"
        );
        assert_eq!(result.actual, "directory content differs from source");
    }

    #[test]
    fn detect_language_rs() {
        assert_eq!(detect_language(std::path::Path::new("main.rs")), "rs");
    }

    #[test]
    fn detect_language_py() {
        assert_eq!(detect_language(std::path::Path::new("script.py")), "py");
    }

    #[test]
    fn detect_language_sh() {
        assert_eq!(detect_language(std::path::Path::new("run.sh")), "sh");
    }

    #[test]
    fn detect_language_yaml() {
        assert_eq!(detect_language(std::path::Path::new("config.yaml")), "yaml");
        assert_eq!(detect_language(std::path::Path::new("other.yml")), "yml");
    }

    #[test]
    fn detect_language_toml() {
        assert_eq!(detect_language(std::path::Path::new("Cargo.toml")), "toml");
    }

    #[test]
    fn detect_language_md() {
        assert_eq!(detect_language(std::path::Path::new("README.md")), "md");
    }

    #[test]
    fn detect_language_no_extension_returns_txt() {
        assert_eq!(detect_language(std::path::Path::new("Makefile")), "txt");
    }

    #[test]
    fn effective_strategy_tera_forces_copy() {
        let dir = tempfile::tempdir().unwrap();
        let fm = make_manager(dir.path());
        let source = std::path::Path::new("template.conf.tera");
        let result = fm.effective_strategy(source, None);
        assert_eq!(result, FileStrategy::Copy);
    }

    #[test]
    fn effective_strategy_tera_overrides_per_file() {
        let dir = tempfile::tempdir().unwrap();
        let fm = make_manager(dir.path());
        let source = std::path::Path::new("template.conf.tera");
        let result = fm.effective_strategy(source, Some(FileStrategy::Symlink));
        assert_eq!(result, FileStrategy::Copy);
    }

    #[test]
    fn effective_strategy_returns_per_file_when_set() {
        let dir = tempfile::tempdir().unwrap();
        let fm = make_manager(dir.path());
        let source = std::path::Path::new("plain.txt");
        let result = fm.effective_strategy(source, Some(FileStrategy::Hardlink));
        assert_eq!(result, FileStrategy::Hardlink);
    }

    #[test]
    fn effective_strategy_default_when_no_per_file() {
        let dir = tempfile::tempdir().unwrap();
        let fm = make_manager(dir.path());
        let source = std::path::Path::new("plain.txt");
        let result = fm.effective_strategy(source, None);
        assert_eq!(result, FileStrategy::Symlink);
    }

    #[test]
    fn plan_empty_profile_returns_empty_vec() {
        let dir = tempfile::tempdir().unwrap();
        let resolved = make_resolved(FilesSpec::default());
        let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();
        let actions = fm.plan(&resolved.merged).unwrap();
        assert!(actions.is_empty());
    }

    #[test]
    fn plan_private_file_with_explicit_origin_skips_and_carries_origin() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();
        let target = config_dir.join("secret.txt");

        let resolved = make_resolved(FilesSpec {
            managed: vec![ManagedFileSpec {
                patch: None,
                source: "nonexistent.txt".to_string(),
                target: target.clone(),
                strategy: Some(FileStrategy::Copy),
                private: true,
                origin: Some("acme-corp".to_string()),
                encryption: None,
                permissions: None,
            }],
            permissions: HashMap::new(),
        });
        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let actions = fm.plan(&resolved.merged).unwrap();

        assert_eq!(actions.len(), 1);
        assert!(
            matches!(&actions[0], FileAction::Skip { origin, reason, .. }
                if origin == "acme-corp" && reason.contains("private"))
        );
    }

    #[test]
    fn plan_source_path_traversal_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();
        let target = config_dir.join("target.txt");

        let resolved = make_resolved(FilesSpec {
            managed: vec![spec("../../etc/passwd", target, Some(FileStrategy::Copy))],
            permissions: HashMap::new(),
        });
        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let err = fm.plan(&resolved.merged).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("escapes root") || msg.contains(".."),
            "error should describe traversal, got: {msg}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn plan_symlink_existing_wrong_link_produces_update() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        let files_dir = config_dir.join("files");
        fs::create_dir_all(&files_dir).unwrap();
        fs::write(files_dir.join("real.txt"), "content").unwrap();

        let target = config_dir.join("output").join("real.txt");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        // Create a symlink pointing somewhere else
        let other = config_dir.join("other.txt");
        fs::write(&other, "other").unwrap();
        std::os::unix::fs::symlink(&other, &target).unwrap();

        let resolved = make_resolved(FilesSpec {
            managed: vec![spec(
                "files/real.txt",
                target.clone(),
                Some(FileStrategy::Symlink),
            )],
            permissions: HashMap::new(),
        });
        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let actions = fm.plan(&resolved.merged).unwrap();

        assert_eq!(actions.len(), 1);
        assert!(
            matches!(&actions[0], FileAction::Update { target: t, .. } if *t == target),
            "wrong symlink target should produce Update, got: {:?}",
            actions
        );
    }

    #[test]
    #[cfg(unix)]
    fn plan_symlink_is_current_with_permissions_mismatch_produces_set_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        let files_dir = config_dir.join("files");
        fs::create_dir_all(&files_dir).unwrap();
        let src = files_dir.join("key.txt");
        fs::write(&src, "secret").unwrap();

        let target = config_dir.join("output").join("key.txt");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&src, &target).unwrap();
        // Give the symlink target file a permissive mode
        fs::set_permissions(&src, fs::Permissions::from_mode(0o644)).unwrap();

        let mut permissions = HashMap::new();
        permissions.insert(target.display().to_string(), "600".to_string());

        let resolved = make_resolved(FilesSpec {
            managed: vec![spec(
                "files/key.txt",
                target.clone(),
                Some(FileStrategy::Symlink),
            )],
            permissions,
        });
        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let actions = fm.plan(&resolved.merged).unwrap();

        assert_eq!(actions.len(), 1);
        assert!(
            matches!(&actions[0], FileAction::SetPermissions { target: t, mode: 0o600, .. } if *t == target),
            "correct symlink with wrong permissions should produce SetPermissions, got: {:?}",
            actions
        );
    }

    #[test]
    #[cfg(unix)]
    fn plan_copy_content_match_with_permissions_mismatch_produces_set_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        let files_dir = config_dir.join("files");
        fs::create_dir_all(&files_dir).unwrap();
        fs::write(files_dir.join("cfg.txt"), "same content").unwrap();

        let target = config_dir.join("output").join("cfg.txt");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, "same content").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();

        let mut permissions = HashMap::new();
        permissions.insert(target.display().to_string(), "600".to_string());

        let resolved = make_resolved(FilesSpec {
            managed: vec![spec(
                "files/cfg.txt",
                target.clone(),
                Some(FileStrategy::Copy),
            )],
            permissions,
        });
        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let actions = fm.plan(&resolved.merged).unwrap();

        assert_eq!(actions.len(), 1);
        assert!(
            matches!(&actions[0], FileAction::SetPermissions { target: t, mode: 0o600, .. } if *t == target),
            "content match with wrong permissions should produce SetPermissions, got: {:?}",
            actions
        );
    }

    #[test]
    #[cfg(unix)]
    fn plan_copy_update_with_permissions_mismatch_produces_two_actions() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        let files_dir = config_dir.join("files");
        fs::create_dir_all(&files_dir).unwrap();
        fs::write(files_dir.join("cfg.txt"), "new content").unwrap();

        let target = config_dir.join("output").join("cfg.txt");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, "old content").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();

        let mut permissions = HashMap::new();
        permissions.insert(target.display().to_string(), "600".to_string());

        let resolved = make_resolved(FilesSpec {
            managed: vec![spec(
                "files/cfg.txt",
                target.clone(),
                Some(FileStrategy::Copy),
            )],
            permissions,
        });
        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let actions = fm.plan(&resolved.merged).unwrap();

        assert_eq!(actions.len(), 2, "expected Update + SetPermissions");
        assert!(
            matches!(&actions[0], FileAction::Update { target: t, .. } if *t == target),
            "first action should be Update, got: {:?}",
            actions[0]
        );
        assert!(
            matches!(&actions[1], FileAction::SetPermissions { target: t, mode: 0o600, .. } if *t == target),
            "second action should be SetPermissions, got: {:?}",
            actions[1]
        );
    }

    #[test]
    fn plan_encryption_always_with_symlink_strategy_errors() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        let files_dir = config_dir.join("files");
        fs::create_dir_all(&files_dir).unwrap();
        fs::write(files_dir.join("enc.txt"), "data").unwrap();

        let target = config_dir.join("output").join("enc.txt");

        let resolved = make_resolved(FilesSpec {
            managed: vec![ManagedFileSpec {
                patch: None,
                source: "files/enc.txt".to_string(),
                target,
                strategy: Some(FileStrategy::Symlink),
                private: false,
                origin: None,
                encryption: Some(EncryptionSpec {
                    backend: "sops".to_string(),
                    mode: EncryptionMode::Always,
                }),
                permissions: None,
            }],
            permissions: HashMap::new(),
        });
        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let err = fm.plan(&resolved.merged).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Always") || msg.contains("incompatible"),
            "error should mention encryption incompatibility, got: {msg}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn check_permissions_per_file_field_takes_priority() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        let target = config_dir.join("file.txt");
        fs::write(&target, "data").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();

        // Per-file permission on managed spec (not in profile.permissions map)
        let managed = ManagedFileSpec {
            patch: None,
            source: "file.txt".to_string(),
            target: target.clone(),
            strategy: Some(FileStrategy::Copy),
            private: false,
            origin: None,
            encryption: None,
            permissions: Some("700".to_string()),
        };
        let resolved = make_resolved(FilesSpec {
            managed: vec![managed.clone()],
            permissions: HashMap::new(),
        });
        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let action = fm
            .check_permissions(&target, &managed, &resolved.merged)
            .unwrap();

        assert!(action.is_some());
        assert!(
            matches!(
                action.unwrap(),
                FileAction::SetPermissions { mode: 0o700, .. }
            ),
            "per-file permissions field should produce SetPermissions 700"
        );
    }

    #[test]
    #[cfg(unix)]
    fn check_permissions_target_nonexistent_emits_set_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        // Target does not exist yet
        let target = config_dir.join("newfile.txt");

        let mut permissions = HashMap::new();
        permissions.insert(target.display().to_string(), "600".to_string());

        let managed = ManagedFileSpec {
            patch: None,
            source: "newfile.txt".to_string(),
            target: target.clone(),
            strategy: Some(FileStrategy::Copy),
            private: false,
            origin: None,
            encryption: None,
            permissions: None,
        };
        let resolved = make_resolved(FilesSpec {
            managed: vec![managed.clone()],
            permissions,
        });
        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let action = fm
            .check_permissions(&target, &managed, &resolved.merged)
            .unwrap();

        assert!(
            action.is_some(),
            "nonexistent target should still emit SetPermissions"
        );
        assert!(matches!(
            action.unwrap(),
            FileAction::SetPermissions { mode: 0o600, .. }
        ));
    }

    #[test]
    #[cfg(unix)]
    fn check_permissions_invalid_mode_string_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        let target = config_dir.join("file.txt");
        fs::write(&target, "data").unwrap();

        let managed = ManagedFileSpec {
            patch: None,
            source: "file.txt".to_string(),
            target: target.clone(),
            strategy: Some(FileStrategy::Copy),
            private: false,
            origin: None,
            encryption: None,
            permissions: Some("not-octal".to_string()),
        };
        let resolved = make_resolved(FilesSpec {
            managed: vec![managed.clone()],
            permissions: HashMap::new(),
        });
        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let err = fm
            .check_permissions(&target, &managed, &resolved.merged)
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("invalid permission mode"),
            "error should describe invalid mode, got: {msg}"
        );
    }

    #[test]
    fn diff_empty_profile_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let resolved = make_resolved(FilesSpec::default());
        let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();
        let printer = Printer::for_test().0;
        let records = fm.diff(&resolved.merged, &printer).unwrap();
        assert!(records.is_empty());
    }

    #[test]
    fn diff_missing_source_warns_and_continues() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();
        let target = config_dir.join("target.txt");

        let resolved = make_resolved(FilesSpec {
            managed: vec![spec("nonexistent.txt", target, Some(FileStrategy::Copy))],
            permissions: HashMap::new(),
        });
        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
        let records = fm.diff(&resolved.merged, &printer).unwrap();

        assert!(
            records.iter().all(|r| !r.matches),
            "an unresolvable source is drift, matching what verify reports"
        );
        let output = cfgd_core::test_helpers::captured_text(&buf);
        assert!(
            output.contains("Source not found") || output.contains("nonexistent"),
            "output should mention missing source, got: {output}"
        );
    }

    /// Managed-file spec for a `Patch` entry with the given `ensure` YAML.
    fn patch_spec(target: std::path::PathBuf, ensure: &str) -> ManagedFileSpec {
        let mut managed = spec("", target, Some(FileStrategy::Patch));
        managed.patch = Some(cfgd_core::config::PatchSpec {
            format: None,
            ensure: Some(serde_yaml::from_str(ensure).unwrap()),
            script: None,
            blocked_by: None,
        });
        managed
    }

    #[test]
    fn plan_patch_creates_a_missing_target() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();
        let target = config_dir.join("settings.json");

        let resolved = make_resolved(FilesSpec {
            managed: vec![patch_spec(target.clone(), "telemetry: false")],
            permissions: HashMap::new(),
        });
        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let actions = fm.plan(&resolved.merged).unwrap();

        assert_eq!(actions.len(), 1, "expected one action, got: {actions:?}");
        match &actions[0] {
            FileAction::Create {
                target: t,
                strategy,
                source,
                source_hash,
                patch,
                ..
            } => {
                assert_eq!(t, &target);
                assert_eq!(*strategy, FileStrategy::Patch);
                assert_eq!(source, &std::path::PathBuf::new(), "Patch has no source");
                assert!(
                    source_hash.is_none(),
                    "apply re-evaluates from live content"
                );
                assert!(patch.is_some(), "the merge spec must reach apply");
            }
            other => panic!("expected Create, got: {other:?}"),
        }
        assert!(!target.exists(), "plan must not write the target");
    }

    #[test]
    fn plan_patch_updates_a_drifted_target_with_a_current_to_merged_diff() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();
        let target = config_dir.join("settings.json");
        fs::write(&target, "{\n  \"keep\": 1\n}\n").unwrap();

        let resolved = make_resolved(FilesSpec {
            managed: vec![patch_spec(target.clone(), "telemetry: false")],
            permissions: HashMap::new(),
        });
        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let actions = fm.plan(&resolved.merged).unwrap();

        match &actions[0] {
            FileAction::Update { diff, .. } => {
                assert!(
                    diff.contains("+  \"telemetry\": false"),
                    "diff must show the merged addition, got: {diff}"
                );
                assert!(
                    diff.contains("+  \"keep\": 1,"),
                    "an untouched key must survive into the merged content, got: {diff}"
                );
            }
            other => panic!("expected Update, got: {other:?}"),
        }
    }

    #[test]
    fn plan_patch_emits_no_action_when_the_target_already_satisfies_the_spec() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();
        let target = config_dir.join("settings.json");
        fs::write(&target, "{\n  \"telemetry\": false\n}\n").unwrap();

        let resolved = make_resolved(FilesSpec {
            managed: vec![patch_spec(target, "telemetry: false")],
            permissions: HashMap::new(),
        });
        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let actions = fm.plan(&resolved.merged).unwrap();

        assert!(
            actions.is_empty(),
            "a converged Patch target is a no-op, got: {actions:?}"
        );
    }

    #[test]
    fn plan_patch_without_a_patch_block_errors() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();
        let target = config_dir.join("settings.json");

        let resolved = make_resolved(FilesSpec {
            managed: vec![spec("", target, Some(FileStrategy::Patch))],
            permissions: HashMap::new(),
        });
        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let err = fm.plan(&resolved.merged).unwrap_err();
        assert!(
            err.to_string().contains("requires a 'patch' block"),
            "expected a missing-patch-block error, got: {err}"
        );
    }

    #[test]
    fn effective_strategy_keeps_patch_for_a_tera_source() {
        let dir = tempfile::tempdir().unwrap();
        let fm = make_manager(dir.path());
        assert_eq!(
            fm.effective_strategy(
                std::path::Path::new("shell/.zshrc.tera"),
                Some(FileStrategy::Patch)
            ),
            FileStrategy::Patch,
            "a declared Patch must never be downgraded to a whole-file Copy"
        );
    }

    #[test]
    fn diff_patch_renders_the_merge_and_reports_drift() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();
        let target = config_dir.join("settings.json");
        fs::write(&target, "{\n  \"keep\": 1\n}\n").unwrap();

        let resolved = make_resolved(FilesSpec {
            managed: vec![patch_spec(target, "telemetry: false")],
            permissions: HashMap::new(),
        });
        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
        assert!(
            fm.diff(&resolved.merged, &printer)
                .unwrap()
                .iter()
                .any(|r| !r.matches)
        );
        let output = cfgd_core::test_helpers::captured_text(&buf);
        assert!(
            output.contains("telemetry"),
            "diff must render the merged content, got: {output}"
        );
    }

    #[test]
    fn diff_patch_renders_nothing_when_converged() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();
        let target = config_dir.join("settings.json");
        fs::write(&target, "{\n  \"telemetry\": false\n}\n").unwrap();

        let resolved = make_resolved(FilesSpec {
            managed: vec![patch_spec(target, "telemetry: false")],
            permissions: HashMap::new(),
        });
        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
        assert!(
            fm.diff(&resolved.merged, &printer)
                .unwrap()
                .iter()
                .all(|r| r.matches)
        );
        assert!(
            cfgd_core::test_helpers::captured_text(&buf).is_empty(),
            "a converged file prints nothing"
        );
    }

    #[test]
    fn file_drift_results_patch_reports_convergence() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();
        let drifted = config_dir.join("drifted.json");
        let converged = config_dir.join("converged.json");
        let missing = config_dir.join("missing.json");
        fs::write(&drifted, "{\n  \"keep\": 1\n}\n").unwrap();
        fs::write(&converged, "{\n  \"telemetry\": false\n}\n").unwrap();

        let resolved = make_resolved(FilesSpec {
            managed: vec![
                patch_spec(drifted, "telemetry: false"),
                patch_spec(converged, "telemetry: false"),
                patch_spec(missing, "telemetry: false"),
            ],
            permissions: HashMap::new(),
        });
        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let results = fm.file_drift_results(&resolved.merged).unwrap();

        assert_eq!(results.len(), 3);
        assert!(!results[0].matches);
        assert_eq!(results[0].actual, "content differs from patch spec");
        assert!(results[1].matches);
        assert!(!results[2].matches);
        assert_eq!(results[2].actual, "missing");
    }

    #[test]
    fn file_drift_results_reports_an_unevaluable_patch_as_drift_not_an_error() {
        // A read-only scan covers every resource: one target cfgd cannot parse
        // must be reported as drift, not abort the run and hide the rest.
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();
        let broken = config_dir.join("broken.json");
        let converged = config_dir.join("converged.json");
        fs::write(&broken, "{ this is not json").unwrap();
        fs::write(&converged, "{\n  \"telemetry\": false\n}\n").unwrap();

        let resolved = make_resolved(FilesSpec {
            managed: vec![
                patch_spec(broken, "telemetry: false"),
                patch_spec(converged, "telemetry: false"),
            ],
            permissions: HashMap::new(),
        });
        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let results = fm
            .file_drift_results(&resolved.merged)
            .expect("one unevaluable file must not fail the whole scan");

        assert_eq!(results.len(), 2, "every file still reports a result");
        assert!(!results[0].matches);
        assert!(
            results[0].actual.starts_with("cannot evaluate patch spec:"),
            "the failure is surfaced per-file, got: {}",
            results[0].actual
        );
        assert!(
            !results[0].actual.contains('\n'),
            "the detail is collapsed to one line, got: {}",
            results[0].actual
        );
        assert!(results[1].matches, "unrelated results stay visible");
    }

    #[test]
    fn diff_reports_an_unevaluable_patch_without_aborting() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();
        let broken = config_dir.join("broken.json");
        fs::write(&broken, "{ this is not json").unwrap();

        let resolved = make_resolved(FilesSpec {
            managed: vec![patch_spec(broken, "telemetry: false")],
            permissions: HashMap::new(),
        });
        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
        let records = fm
            .diff(&resolved.merged, &printer)
            .expect("an unevaluable file must not fail the diff");

        assert!(
            records.iter().any(|r| !r.matches),
            "an unevaluable Patch file counts as drift"
        );
        let output = cfgd_core::test_helpers::captured_text(&buf);
        assert!(
            output.contains("cannot evaluate patch spec"),
            "the reason is printed, got: {output}"
        );
    }
}
