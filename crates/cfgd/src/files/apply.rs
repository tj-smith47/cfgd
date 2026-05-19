use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use cfgd_core::config::FileStrategy;
use cfgd_core::errors::{FileError, Result};
use cfgd_core::output_v2::Printer;
use cfgd_core::providers::{FileAction, FileDiff, FileDiffKind, FileEntry, FileLayer, FileTree};

use super::template::is_tera_template;

/// Implement the FileManager trait for CfgdFileManager.
impl cfgd_core::providers::FileManager for super::CfgdFileManager {
    fn scan_source(&self, layers: &[FileLayer]) -> Result<FileTree> {
        let mut files = BTreeMap::new();

        for layer in layers {
            if !layer.source_dir.exists() {
                continue;
            }

            scan_directory(
                &layer.source_dir,
                &layer.source_dir,
                &layer.origin_source,
                &mut files,
            )?;
        }

        Ok(FileTree { files })
    }

    fn scan_target(&self, paths: &[PathBuf]) -> Result<FileTree> {
        let mut files = BTreeMap::new();

        for path in paths {
            if !path.exists() {
                continue;
            }

            let content = fs::read_to_string(path).map_err(|e| FileError::Io {
                path: path.clone(),
                source: e,
            })?;

            let hash = cfgd_core::sha256_hex(content.as_bytes());
            let metadata = fs::metadata(path).map_err(|e| FileError::Io {
                path: path.clone(),
                source: e,
            })?;

            files.insert(
                path.clone(),
                FileEntry {
                    content_hash: hash,
                    permissions: cfgd_core::file_permissions_mode(&metadata),
                    is_template: false,
                    source_path: path.clone(),
                    origin_source: "local".to_string(),
                },
            );
        }

        Ok(FileTree { files })
    }

    fn diff(&self, source: &FileTree, target: &FileTree) -> Result<Vec<FileDiff>> {
        let mut diffs = Vec::new();

        for (path, source_entry) in &source.files {
            if let Some(target_entry) = target.files.get(path) {
                if source_entry.content_hash != target_entry.content_hash {
                    diffs.push(FileDiff {
                        target: path.clone(),
                        kind: FileDiffKind::Modified {
                            source: source_entry.source_path.clone(),
                            diff: String::new(),
                        },
                    });
                } else if source_entry.permissions != target_entry.permissions {
                    if let (Some(desired), Some(current)) =
                        (source_entry.permissions, target_entry.permissions)
                    {
                        diffs.push(FileDiff {
                            target: path.clone(),
                            kind: FileDiffKind::PermissionsChanged { current, desired },
                        });
                    }
                } else {
                    diffs.push(FileDiff {
                        target: path.clone(),
                        kind: FileDiffKind::Unchanged,
                    });
                }
            } else {
                diffs.push(FileDiff {
                    target: path.clone(),
                    kind: FileDiffKind::Created {
                        source: source_entry.source_path.clone(),
                    },
                });
            }
        }

        // Deletions: files in target but not in source
        for path in target.files.keys() {
            if !source.files.contains_key(path) {
                diffs.push(FileDiff {
                    target: path.clone(),
                    kind: FileDiffKind::Deleted,
                });
            }
        }

        Ok(diffs)
    }

    fn apply(&self, actions: &[FileAction], printer: &Printer) -> Result<()> {
        let pb = printer.progress_bar(actions.len() as u64, "Applying files");

        for action in actions {
            match action {
                FileAction::Create {
                    source,
                    target,
                    strategy,
                    ..
                }
                | FileAction::Update {
                    source,
                    target,
                    strategy,
                    ..
                } => {
                    let file_origin = match action {
                        FileAction::Create { origin, .. } | FileAction::Update { origin, .. } => {
                            if origin == "local" {
                                None
                            } else {
                                Some(origin.as_str())
                            }
                        }
                        _ => None,
                    };

                    // Ensure parent directory exists and is writable
                    ensure_target_writable(target)?;

                    // Remove existing target (symlink, file, etc.) before deploying
                    if target.exists() || target.symlink_metadata().is_ok() {
                        fs::remove_file(target).map_err(|e| FileError::Io {
                            path: target.clone(),
                            source: e,
                        })?;
                    }

                    match strategy {
                        FileStrategy::Symlink => {
                            cfgd_core::create_symlink(source, target).map_err(|e| {
                                FileError::Io {
                                    path: target.clone(),
                                    source: e,
                                }
                            })?;
                        }
                        FileStrategy::Hardlink => {
                            fs::hard_link(source, target).map_err(|e| FileError::Io {
                                path: target.clone(),
                                source: e,
                            })?;
                        }
                        FileStrategy::Copy | FileStrategy::Template => {
                            let mut content = if is_tera_template(source) {
                                self.render_template(source, file_origin)?
                            } else {
                                fs::read_to_string(source).map_err(|e| FileError::Io {
                                    path: source.clone(),
                                    source: e,
                                })?
                            };

                            // TOCTOU check: verify source hasn't changed since planning
                            let expected_hash = match action {
                                FileAction::Create { source_hash, .. }
                                | FileAction::Update { source_hash, .. } => source_hash.as_deref(),
                                _ => None,
                            };
                            if let Some(plan_hash) = expected_hash {
                                let current_hash = cfgd_core::sha256_hex(content.as_bytes());
                                if current_hash != plan_hash {
                                    return Err(FileError::SourceChanged {
                                        path: source.clone(),
                                    }
                                    .into());
                                }
                            }

                            if content.contains("${secret:") {
                                let provider_refs: Vec<&dyn cfgd_core::providers::SecretProvider> =
                                    self.secret_providers.iter().map(|p| p.as_ref()).collect();
                                content = crate::secrets::resolve_secret_refs(
                                    &content,
                                    &provider_refs,
                                    self.secret_backend.as_deref(),
                                    &self.config_dir,
                                )?;
                            }

                            cfgd_core::atomic_write(target, content.as_bytes()).map_err(|e| {
                                FileError::Io {
                                    path: target.clone(),
                                    source: e,
                                }
                            })?;
                        }
                    }
                }
                FileAction::Delete { target, .. } => {
                    if target.exists() || target.symlink_metadata().is_ok() {
                        fs::remove_file(target).map_err(|e| FileError::Io {
                            path: target.clone(),
                            source: e,
                        })?;
                    }
                }
                FileAction::SetPermissions { target, mode, .. } => {
                    set_permissions(target, *mode)?;
                }
                FileAction::Skip { .. } => {}
            }
            pb.inc(1);
        }

        pb.finish();
        Ok(())
    }
}

/// Recursively scan a directory and add file entries to the map.
/// Skips symlinks in the source tree to prevent symlink attacks and loops.
fn scan_directory(
    dir: &Path,
    root: &Path,
    origin: &str,
    files: &mut BTreeMap<PathBuf, FileEntry>,
) -> Result<()> {
    let entries = fs::read_dir(dir).map_err(|e| FileError::Io {
        path: dir.to_path_buf(),
        source: e,
    })?;

    for entry in entries {
        let entry = entry.map_err(|e| FileError::Io {
            path: dir.to_path_buf(),
            source: e,
        })?;

        let path = entry.path();

        // Skip symlinks in source tree — prevents symlink attacks and infinite loops
        let file_type = entry.file_type().map_err(|e| FileError::Io {
            path: path.clone(),
            source: e,
        })?;
        if file_type.is_symlink() {
            continue;
        }

        if file_type.is_dir() {
            scan_directory(&path, root, origin, files)?;
            continue;
        }

        let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();

        let content = fs::read_to_string(&path).map_err(|e| FileError::Io {
            path: path.clone(),
            source: e,
        })?;

        let hash = cfgd_core::sha256_hex(content.as_bytes());
        let metadata = fs::metadata(&path).map_err(|e| FileError::Io {
            path: path.clone(),
            source: e,
        })?;

        files.insert(
            relative,
            FileEntry {
                content_hash: hash,
                permissions: cfgd_core::file_permissions_mode(&metadata),
                is_template: is_tera_template(&path),
                source_path: path,
                origin_source: origin.to_string(),
            },
        );
    }

    Ok(())
}

/// Ensure the target's parent directory exists and the target is writable.
pub(super) fn ensure_target_writable(target: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| FileError::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
        if parent.exists() {
            let meta = fs::metadata(parent).map_err(|e| FileError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
            if meta.permissions().readonly() {
                return Err(FileError::TargetNotWritable {
                    path: target.to_path_buf(),
                }
                .into());
            }
        }
    }
    // Check existing target (real files — symlinks are checked via symlink_metadata)
    if target.exists() {
        let meta = fs::metadata(target).map_err(|e| FileError::Io {
            path: target.to_path_buf(),
            source: e,
        })?;
        if meta.permissions().readonly() {
            return Err(FileError::TargetNotWritable {
                path: target.to_path_buf(),
            }
            .into());
        }
    }
    Ok(())
}

/// Set file permissions (Unix mode bits). No-op on Windows.
pub(super) fn set_permissions(path: &Path, mode: u32) -> Result<()> {
    cfgd_core::set_file_permissions(path, mode).map_err(|e| {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            FileError::PermissionDenied {
                path: path.to_path_buf(),
                mode,
            }
            .into()
        } else {
            FileError::Io {
                path: path.to_path_buf(),
                source: e,
            }
            .into()
        }
    })
}
