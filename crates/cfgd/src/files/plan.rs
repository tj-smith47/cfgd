use std::fs;
use std::path::{Path, PathBuf};

use similar::TextDiff;

use cfgd_core::config::{EncryptionMode, FileStrategy, ManagedFileSpec, MergedProfile};
use cfgd_core::errors::{FileError, Result};
use cfgd_core::expand_tilde;
use cfgd_core::output::Printer;
use cfgd_core::providers::FileAction;

use super::is_file_encrypted;
use super::template::is_tera_template;

impl super::CfgdFileManager {
    /// Resolve the effective strategy for a managed file.
    /// Template files always use Copy (can't symlink unrendered templates).
    pub(super) fn effective_strategy(
        &self,
        source: &Path,
        per_file: Option<FileStrategy>,
    ) -> FileStrategy {
        if is_tera_template(source) {
            return FileStrategy::Copy;
        }
        per_file.unwrap_or(self.global_strategy)
    }

    /// Build a plan of file actions by comparing desired state (from profile) to actual state (on disk).
    pub fn plan(&self, profile: &MergedProfile) -> Result<Vec<FileAction>> {
        let mut actions = Vec::new();

        for managed in &profile.files.managed {
            let source_path = self.resolve_source_path(&managed.source)?;
            let target_path = expand_tilde(&managed.target);

            if !source_path.exists() {
                if managed.private {
                    // Private files are local-only — skip silently on other machines
                    actions.push(FileAction::Skip {
                        target: target_path,
                        reason: "private (local only)".to_string(),
                        origin: managed
                            .origin
                            .clone()
                            .unwrap_or_else(|| "local".to_string()),
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
                let encrypted = is_file_encrypted(&source_path, &enc.backend)?;
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
                .unwrap_or_else(|| "local".to_string());

            // For symlink/hardlink: check if the target is already the correct link
            if matches!(strategy, FileStrategy::Symlink | FileStrategy::Hardlink) {
                let is_current = match strategy {
                    FileStrategy::Symlink => target_path
                        .read_link()
                        .map(|link| link == source_path)
                        .unwrap_or(false),
                    FileStrategy::Hardlink => cfgd_core::is_same_inode(&source_path, &target_path),
                    _ => false,
                };

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
                    });
                } else {
                    actions.push(FileAction::Create {
                        source: source_path.clone(),
                        target: target_path.clone(),
                        origin,
                        strategy,
                        source_hash: None,
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
                        .header(
                            &target_path.display().to_string(),
                            &source_path.display().to_string(),
                        )
                        .to_string();

                    let content_hash = cfgd_core::sha256_hex(rendered_content.as_bytes());
                    actions.push(FileAction::Update {
                        source: source_path.clone(),
                        target: target_path.clone(),
                        diff: unified,
                        origin,
                        strategy,
                        source_hash: Some(content_hash),
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
                });

                if let Some(action) = self.check_permissions(&target_path, managed, profile)? {
                    actions.push(action);
                }
            }
        }

        Ok(actions)
    }

    /// Show diffs for all managed files, with syntax highlighting.
    /// Render and print file diffs for the profile. Returns `true` when at
    /// least one file differs from its target (or is missing); `false` when
    /// every managed file matches desired state. The caller uses this to
    /// decide whether to emit `ExitCode::DriftDetected`.
    pub fn diff(&self, profile: &MergedProfile, printer: &Printer) -> Result<bool> {
        let mut has_diffs = false;

        for managed in &profile.files.managed {
            let source_path = self.resolve_source_path(&managed.source)?;
            let target_path = expand_tilde(&managed.target);

            if !source_path.exists() {
                printer.warning(&format!("Source not found: {}", source_path.display()));
                continue;
            }

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

                if rendered_content != target_content {
                    has_diffs = true;
                    printer.subheader(&format!("{}", target_path.display()));

                    printer.diff(&target_content, &rendered_content);
                    printer.newline();
                }
            } else {
                has_diffs = true;
                printer.subheader(&format!("{} (new file)", target_path.display()));
                let lang = detect_language(&target_path);
                printer.syntax_highlight(&rendered_content, &lang);
                printer.newline();
            }
        }

        if !has_diffs {
            printer.success("All files match desired state");
        }

        Ok(has_diffs)
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
            // Skip generating SetPermissions actions and log a one-time info message.
            #[cfg(windows)]
            {
                use std::sync::Once;
                static WARN_ONCE: Once = Once::new();
                WARN_ONCE.call_once(|| {
                    tracing::info!(
                        "file permissions are not applicable on Windows (NTFS uses inherited ACLs); \
                         permissions settings will be ignored"
                    );
                });
                let _ = mode_str;
                return Ok(None);
            }
            #[cfg(not(windows))]
            {
                let desired_mode =
                    u32::from_str_radix(mode_str, 8).map_err(|_| FileError::TemplateError {
                        path: target.to_path_buf(),
                        message: format!("invalid permission mode: {}", mode_str),
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
                            origin: "local".to_string(),
                        }));
                    }
                } else {
                    // Target doesn't exist yet (will be created); emit SetPermissions
                    // so that apply sets the correct mode after creating the file.
                    return Ok(Some(FileAction::SetPermissions {
                        target: target.to_path_buf(),
                        mode: desired_mode,
                        origin: "local".to_string(),
                    }));
                }
            }
        }

        Ok(None)
    }

    /// Resolve a source path relative to the config directory.
    pub(super) fn resolve_source_path(
        &self,
        source: &str,
    ) -> std::result::Result<PathBuf, FileError> {
        let resolved = cfgd_core::resolve_relative_path(&PathBuf::from(source), &self.config_dir)
            .map_err(|_| FileError::PathTraversal {
            path: self.config_dir.join(source),
            root: self.config_dir.clone(),
        })?;
        // If the path exists, do a full canonicalization check
        if resolved.exists() {
            cfgd_core::validate_path_within(&resolved, &self.config_dir).map_err(|_| {
                FileError::PathTraversal {
                    path: resolved.clone(),
                    root: self.config_dir.clone(),
                }
            })?;
        }
        Ok(resolved)
    }
}

/// Detect language from file extension for syntax highlighting.
pub(super) fn detect_language(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("txt")
        .to_string()
}
