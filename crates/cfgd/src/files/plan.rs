use std::fs;
use std::path::{Path, PathBuf};

use similar::TextDiff;

use cfgd_core::PathDisplayExt;
use cfgd_core::config::{EncryptionMode, FileStrategy, ManagedFileSpec, MergedProfile};
use cfgd_core::errors::{FileError, Result};
use cfgd_core::expand_tilde;
use cfgd_core::output::{Printer, Role};
use cfgd_core::providers::FileAction;

use super::is_file_encrypted;
use super::template::is_tera_template;

/// Content-drift outcome for a single managed file, produced by
/// [`super::CfgdFileManager::file_drift_results`]. Carries enough detail to
/// build a `VerifyResult` without re-reading the file.
#[derive(Debug, Clone)]
pub(crate) struct FileDriftResult {
    pub target: String,
    pub matches: bool,
    pub expected: String,
    pub actual: String,
}

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
                printer.status_simple(
                    Role::Warn,
                    format!("Source not found: {}", source_path.posix()),
                );
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
                    printer.status_simple(Role::Info, target_path.display_posix());
                    printer.diff(&target_content, &rendered_content);
                }
            } else {
                has_diffs = true;
                printer.status_simple(Role::Info, format!("{} (new file)", target_path.posix()));
                let lang = detect_language(&target_path);
                printer.syntax_highlight(&rendered_content, &lang);
            }
        }

        Ok(has_diffs)
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
            let source_path = self.resolve_source_path(&managed.source)?;
            let target_path = expand_tilde(&managed.target);
            let target_id = target_path.display_posix();

            if !source_path.exists() {
                results.push(FileDriftResult {
                    target: target_id,
                    matches: false,
                    expected: "managed source present".to_string(),
                    actual: format!("source not found: {}", source_path.posix()),
                });
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
                let matches = rendered_content == target_content;
                results.push(FileDriftResult {
                    target: target_id,
                    matches,
                    expected: "content matches source".to_string(),
                    actual: if matches {
                        "content matches source".to_string()
                    } else {
                        "content differs from source".to_string()
                    },
                });
            } else {
                results.push(FileDriftResult {
                    target: target_id,
                    matches: false,
                    expected: "present".to_string(),
                    actual: "missing".to_string(),
                });
            }
        }

        Ok(results)
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
    fn resolve_source_path(&self, source: &str) -> std::result::Result<PathBuf, FileError> {
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
    use cfgd_core::providers::FileAction;

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
            &actions[0]
        );
        assert!(
            matches!(&actions[1], FileAction::SetPermissions { target: t, mode: 0o600, .. } if *t == target),
            "second action should be SetPermissions, got: {:?}",
            &actions[1]
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
        let printer = Printer::new(Verbosity::Quiet);
        let has_diff = fm.diff(&resolved.merged, &printer).unwrap();
        assert!(!has_diff);
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
        let has_diff = fm.diff(&resolved.merged, &printer).unwrap();

        assert!(!has_diff, "missing source should not count as a diff");
        let output = buf.lock().unwrap();
        assert!(
            output.contains("Source not found") || output.contains("nonexistent"),
            "output should mention missing source, got: {output}"
        );
    }
}
