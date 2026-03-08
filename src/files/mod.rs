#![allow(dead_code)]

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use similar::TextDiff;
use tera::{Context, Tera};

use crate::config::{ManagedFileSpec, MergedProfile, ResolvedProfile};
use crate::errors::{FileError, Result};
use crate::output::Printer;
use crate::providers::{FileAction, FileDiff, FileDiffKind, FileEntry, FileLayer, FileTree};

/// Check if a path is a Tera template file (has .tera extension).
pub(crate) fn is_tera_template(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("tera")
}

/// Concrete FileManager implementation for cfgd.
/// Manages files declared in profiles: copy, template, diff, permissions.
pub struct CfgdFileManager {
    tera: Tera,
    context: Context,
    config_dir: PathBuf,
    secret_backend: Option<Box<dyn crate::providers::SecretBackend>>,
    secret_providers: Vec<Box<dyn crate::providers::SecretProvider>>,
}

impl CfgdFileManager {
    /// Create a new file manager.
    /// `config_dir` is the directory containing cfgd.yaml (used to resolve relative source paths).
    /// `resolved` is the fully resolved profile with merged variables.
    pub fn new(config_dir: &Path, resolved: &ResolvedProfile) -> Result<Self> {
        let mut tera = Tera::default();
        tera.autoescape_on(vec![]);

        let mut context = Context::new();

        // Flat profile variables
        for (key, value) in &resolved.merged.variables {
            let val_str = yaml_value_to_json(value);
            context.insert(key, &val_str);
        }

        // System facts as custom values
        context.insert("__os", &std::env::consts::OS);
        context.insert("__arch", &std::env::consts::ARCH);
        context.insert(
            "__hostname",
            &hostname::get()
                .map(|h| h.to_string_lossy().to_string())
                .unwrap_or_default(),
        );

        // Phase 9 prep: empty sources map alongside flat variables
        let sources: HashMap<String, HashMap<String, String>> = HashMap::new();
        context.insert("sources", &sources);

        Ok(Self {
            tera,
            context,
            config_dir: config_dir.to_path_buf(),
            secret_backend: None,
            secret_providers: Vec::new(),
        })
    }

    /// Set secret backend and providers for resolving `${secret:ref}` in templates.
    pub fn set_secret_providers(
        &mut self,
        backend: Option<Box<dyn crate::providers::SecretBackend>>,
        providers: Vec<Box<dyn crate::providers::SecretProvider>>,
    ) {
        self.secret_backend = backend;
        self.secret_providers = providers;
    }

    /// Build a plan of file actions by comparing desired state (from profile) to actual state (on disk).
    pub fn plan(&mut self, profile: &MergedProfile) -> Result<Vec<FileAction>> {
        let mut actions = Vec::new();

        for managed in &profile.files.managed {
            let source_path = self.resolve_source_path(&managed.source);
            let target_path = expand_tilde(&managed.target);

            if !source_path.exists() {
                return Err(FileError::SourceNotFound { path: source_path }.into());
            }

            let rendered_content = if is_tera_template(&source_path) {
                self.render_template(&source_path)?
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
                    // Check permissions
                    if let Some(action) = self.check_permissions(&target_path, managed, profile)? {
                        actions.push(action);
                    }
                    // Content matches — skip
                } else {
                    let diff = TextDiff::from_lines(&target_content, &rendered_content);
                    let unified = diff
                        .unified_diff()
                        .header(
                            &target_path.display().to_string(),
                            &source_path.display().to_string(),
                        )
                        .to_string();

                    actions.push(FileAction::Update {
                        source: source_path.clone(),
                        target: target_path.clone(),
                        diff: unified,
                        origin: "local".to_string(),
                    });

                    // Also check permissions after update
                    if let Some(action) = self.check_permissions(&target_path, managed, profile)? {
                        actions.push(action);
                    }
                }
            } else {
                actions.push(FileAction::Create {
                    source: source_path.clone(),
                    target: target_path.clone(),
                    origin: "local".to_string(),
                });

                // Check if permissions need to be set on create
                if let Some(action) = self.check_permissions(&target_path, managed, profile)? {
                    actions.push(action);
                }
            }
        }

        Ok(actions)
    }

    /// Apply a list of file actions, writing files and setting permissions.
    pub fn apply(
        &mut self,
        actions: &[FileAction],
        profile: &MergedProfile,
        printer: &Printer,
    ) -> Result<()> {
        if actions.is_empty() {
            return Ok(());
        }

        let pb = printer.progress_bar(actions.len() as u64, "Applying files");

        for action in actions {
            match action {
                FileAction::Create { source, target, .. } => {
                    self.write_file(source, target, profile)?;
                    pb.inc(1);
                }
                FileAction::Update { source, target, .. } => {
                    self.write_file(source, target, profile)?;
                    pb.inc(1);
                }
                FileAction::SetPermissions { target, mode, .. } => {
                    set_permissions(target, *mode)?;
                    pb.inc(1);
                }
                FileAction::Delete { target, .. } => {
                    if target.exists() {
                        fs::remove_file(target).map_err(|e| FileError::Io {
                            path: target.clone(),
                            source: e,
                        })?;
                    }
                    pb.inc(1);
                }
                FileAction::Skip { .. } => {
                    pb.inc(1);
                }
            }
        }

        pb.finish_and_clear();
        Ok(())
    }

    /// Show diffs for all managed files, with syntax highlighting.
    pub fn diff(&mut self, profile: &MergedProfile, printer: &Printer) -> Result<()> {
        let mut has_diffs = false;

        for managed in &profile.files.managed {
            let source_path = self.resolve_source_path(&managed.source);
            let target_path = expand_tilde(&managed.target);

            if !source_path.exists() {
                printer.warning(&format!("Source not found: {}", source_path.display()));
                continue;
            }

            let rendered_content = if is_tera_template(&source_path) {
                self.render_template(&source_path)?
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

        Ok(())
    }

    /// Add a file to the source directory and return the ManagedFileSpec to add to the profile.
    pub fn add_file(&self, file_path: &Path, profile_name: &str) -> Result<ManagedFileSpec> {
        let file_path = expand_tilde(file_path);

        if !file_path.exists() {
            return Err(FileError::SourceNotFound { path: file_path }.into());
        }

        // Determine relative source path within the files directory
        let file_name = file_path
            .file_name()
            .ok_or_else(|| FileError::SourceNotFound {
                path: file_path.clone(),
            })?;

        let files_dir = self.config_dir.join("files").join(profile_name);
        fs::create_dir_all(&files_dir).map_err(|e| FileError::Io {
            path: files_dir.clone(),
            source: e,
        })?;

        let dest = files_dir.join(file_name);
        fs::copy(&file_path, &dest).map_err(|e| FileError::Io {
            path: dest.clone(),
            source: e,
        })?;

        let source_relative = format!("files/{}/{}", profile_name, file_name.to_string_lossy());

        Ok(ManagedFileSpec {
            source: source_relative,
            target: file_path,
        })
    }

    /// Render a template and return the content for display (e.g., in plan diffs).
    pub fn render_template_for_display(&mut self, path: &Path) -> Result<String> {
        self.render_template(path)
    }

    /// Render a .tera template file with profile variables and system facts.
    fn render_template(&mut self, path: &Path) -> Result<String> {
        let template_content = fs::read_to_string(path).map_err(|e| FileError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;

        let template_name = path.display().to_string();
        self.tera
            .add_raw_template(&template_name, &template_content)
            .map_err(|e| FileError::TemplateError {
                path: path.to_path_buf(),
                message: format_tera_error(&e),
            })?;

        // Register custom functions
        register_custom_functions(&mut self.tera);

        self.tera
            .render(&template_name, &self.context)
            .map_err(|e| FileError::TemplateError {
                path: path.to_path_buf(),
                message: format_tera_error(&e),
            })
            .map_err(Into::into)
    }

    /// Write a source file to target, rendering templates if needed.
    fn write_file(&mut self, source: &Path, target: &Path, profile: &MergedProfile) -> Result<()> {
        let mut content = if is_tera_template(source) {
            self.render_template(source)?
        } else {
            fs::read_to_string(source).map_err(|e| FileError::Io {
                path: source.to_path_buf(),
                source: e,
            })?
        };

        // Resolve ${secret:ref} placeholders if any secret providers are configured
        if content.contains("${secret:") {
            let provider_refs: Vec<&dyn crate::providers::SecretProvider> =
                self.secret_providers.iter().map(|p| p.as_ref()).collect();
            content = crate::secrets::resolve_secret_refs(
                &content,
                &provider_refs,
                self.secret_backend.as_deref(),
                &self.config_dir,
            )?;
        }

        // Ensure parent directory exists
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| FileError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }

        fs::write(target, &content).map_err(|e| FileError::Io {
            path: target.to_path_buf(),
            source: e,
        })?;

        // Apply permissions from profile if configured
        let target_str = target.display().to_string();
        if let Some(mode_str) = profile.files.permissions.get(&target_str) {
            if let Ok(mode) = u32::from_str_radix(mode_str, 8) {
                set_permissions(target, mode)?;
            }
        }

        Ok(())
    }

    /// Check if permissions need to be changed for a target file.
    fn check_permissions(
        &self,
        target: &Path,
        managed: &ManagedFileSpec,
        profile: &MergedProfile,
    ) -> Result<Option<FileAction>> {
        let target_str = target.display().to_string();

        // Also check the source path as a key (relative path in permissions map)
        let mode_str = profile
            .files
            .permissions
            .get(&target_str)
            .or_else(|| profile.files.permissions.get(&managed.source));

        if let Some(mode_str) = mode_str {
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
                let current_mode = metadata.permissions().mode() & 0o777;

                if current_mode != desired_mode {
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
    fn resolve_source_path(&self, source: &str) -> PathBuf {
        let path = PathBuf::from(source);
        if path.is_absolute() {
            path
        } else {
            self.config_dir.join(source)
        }
    }
}

/// Implement the FileManager trait for CfgdFileManager.
impl crate::providers::FileManager for CfgdFileManager {
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

            let hash = sha256_hash(&content);
            let metadata = fs::metadata(path).map_err(|e| FileError::Io {
                path: path.clone(),
                source: e,
            })?;

            files.insert(
                path.clone(),
                FileEntry {
                    content_hash: hash,
                    permissions: Some(metadata.permissions().mode() & 0o777),
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
                FileAction::Create { source, target, .. }
                | FileAction::Update { source, target, .. } => {
                    if let Some(parent) = target.parent() {
                        fs::create_dir_all(parent).map_err(|e| FileError::Io {
                            path: parent.to_path_buf(),
                            source: e,
                        })?;
                    }
                    fs::copy(source, target).map_err(|e| FileError::Io {
                        path: target.clone(),
                        source: e,
                    })?;
                }
                FileAction::Delete { target, .. } => {
                    if target.exists() {
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

        pb.finish_and_clear();
        Ok(())
    }
}

/// Recursively scan a directory and add file entries to the map.
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

        if path.is_dir() {
            scan_directory(&path, root, origin, files)?;
            continue;
        }

        let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();

        let content = fs::read_to_string(&path).map_err(|e| FileError::Io {
            path: path.clone(),
            source: e,
        })?;

        let hash = sha256_hash(&content);
        let metadata = fs::metadata(&path).map_err(|e| FileError::Io {
            path: path.clone(),
            source: e,
        })?;

        files.insert(
            relative,
            FileEntry {
                content_hash: hash,
                permissions: Some(metadata.permissions().mode() & 0o777),
                is_template: is_tera_template(&path),
                source_path: path,
                origin_source: origin.to_string(),
            },
        );
    }

    Ok(())
}

/// Set file permissions (Unix mode bits).
fn set_permissions(path: &Path, mode: u32) -> Result<()> {
    let perms = fs::Permissions::from_mode(mode);
    fs::set_permissions(path, perms).map_err(|e| {
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

/// Expand ~ to home directory in a path.
pub fn expand_tilde(path: &Path) -> PathBuf {
    let path_str = path.display().to_string();
    if path_str.starts_with("~/") {
        if let Some(home) = dirs_home() {
            return PathBuf::from(path_str.replacen('~', &home, 1));
        }
    }
    path.to_path_buf()
}

/// Get home directory.
fn dirs_home() -> Option<String> {
    std::env::var("HOME").ok()
}

/// Compute SHA256 hash of content.
fn sha256_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Convert a serde_yaml::Value to a string suitable for Tera context.
fn yaml_value_to_json(value: &serde_yaml::Value) -> serde_json::Value {
    match value {
        serde_yaml::Value::Null => serde_json::Value::Null,
        serde_yaml::Value::Bool(b) => serde_json::Value::Bool(*b),
        serde_yaml::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                serde_json::Value::Number(i.into())
            } else if let Some(f) = n.as_f64() {
                serde_json::json!(f)
            } else {
                serde_json::Value::Null
            }
        }
        serde_yaml::Value::String(s) => serde_json::Value::String(s.clone()),
        serde_yaml::Value::Sequence(seq) => {
            serde_json::Value::Array(seq.iter().map(yaml_value_to_json).collect())
        }
        serde_yaml::Value::Mapping(map) => {
            let obj: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .filter_map(|(k, v)| {
                    k.as_str()
                        .map(|key| (key.to_string(), yaml_value_to_json(v)))
                })
                .collect();
            serde_json::Value::Object(obj)
        }
        serde_yaml::Value::Tagged(tagged) => yaml_value_to_json(&tagged.value),
    }
}

/// Format a Tera error with source location details.
fn format_tera_error(err: &tera::Error) -> String {
    let mut msg = err.to_string();
    let mut source = std::error::Error::source(err);
    while let Some(cause) = source {
        msg.push_str(&format!("\n  caused by: {}", cause));
        source = std::error::Error::source(cause);
    }
    msg
}

/// Detect language from file extension for syntax highlighting.
fn detect_language(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("txt")
        .to_string()
}

/// Register custom Tera functions: os(), hostname(), arch(), env(name).
fn register_custom_functions(tera: &mut Tera) {
    tera.register_function(
        "os",
        |_args: &HashMap<String, tera::Value>| -> tera::Result<tera::Value> {
            Ok(tera::Value::String(std::env::consts::OS.to_string()))
        },
    );

    tera.register_function(
        "hostname",
        |_args: &HashMap<String, tera::Value>| -> tera::Result<tera::Value> {
            let name = hostname::get()
                .map(|h| h.to_string_lossy().to_string())
                .unwrap_or_default();
            Ok(tera::Value::String(name))
        },
    );

    tera.register_function(
        "arch",
        |_args: &HashMap<String, tera::Value>| -> tera::Result<tera::Value> {
            Ok(tera::Value::String(std::env::consts::ARCH.to_string()))
        },
    );

    tera.register_function(
        "env",
        |args: &HashMap<String, tera::Value>| -> tera::Result<tera::Value> {
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| tera::Error::msg("env() requires a 'name' argument"))?;
            let value = std::env::var(name).unwrap_or_default();
            Ok(tera::Value::String(value))
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::config::{
        FilesSpec, LayerPolicy, ManagedFileSpec, MergedProfile, ProfileLayer, ProfileSpec,
        ResolvedProfile,
    };

    fn make_resolved_profile(
        variables: HashMap<String, serde_yaml::Value>,
        files: FilesSpec,
    ) -> ResolvedProfile {
        ResolvedProfile {
            layers: vec![ProfileLayer {
                source: "local".to_string(),
                profile_name: "test".to_string(),
                priority: 1000,
                policy: LayerPolicy::Local,
                spec: ProfileSpec::default(),
            }],
            merged: MergedProfile {
                variables,
                files,
                ..Default::default()
            },
        }
    }

    #[test]
    fn expand_tilde_with_home() {
        std::env::set_var("HOME", "/home/testuser");
        let result = expand_tilde(Path::new("~/.config/test"));
        assert_eq!(result, PathBuf::from("/home/testuser/.config/test"));
    }

    #[test]
    fn expand_tilde_absolute_path_unchanged() {
        let result = expand_tilde(Path::new("/etc/config"));
        assert_eq!(result, PathBuf::from("/etc/config"));
    }

    #[test]
    fn sha256_hash_deterministic() {
        let h1 = sha256_hash("hello world");
        let h2 = sha256_hash("hello world");
        assert_eq!(h1, h2);
        assert_ne!(h1, sha256_hash("different"));
    }

    #[test]
    fn yaml_value_to_json_converts_types() {
        assert_eq!(
            yaml_value_to_json(&serde_yaml::Value::String("hello".into())),
            serde_json::Value::String("hello".into())
        );
        assert_eq!(
            yaml_value_to_json(&serde_yaml::Value::Bool(true)),
            serde_json::Value::Bool(true)
        );
        assert_eq!(
            yaml_value_to_json(&serde_yaml::Value::Null),
            serde_json::Value::Null
        );
    }

    #[test]
    fn detect_language_from_extension() {
        assert_eq!(detect_language(Path::new("test.rs")), "rs");
        assert_eq!(detect_language(Path::new("config.yaml")), "yaml");
        assert_eq!(detect_language(Path::new("noext")), "txt");
    }

    #[test]
    fn plan_creates_file_when_target_missing() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        // Create source file
        let files_dir = config_dir.join("files");
        fs::create_dir_all(&files_dir).unwrap();
        fs::write(files_dir.join("test.txt"), "hello world").unwrap();

        let target = config_dir.join("target").join("test.txt");

        let resolved = make_resolved_profile(
            HashMap::new(),
            FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "files/test.txt".to_string(),
                    target: target.clone(),
                }],
                permissions: HashMap::new(),
            },
        );

        let mut fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let actions = fm.plan(&resolved.merged).unwrap();

        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], FileAction::Create { target: t, .. } if *t == target));
    }

    #[test]
    fn plan_skips_when_content_matches() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        // Create source file
        let files_dir = config_dir.join("files");
        fs::create_dir_all(&files_dir).unwrap();
        fs::write(files_dir.join("test.txt"), "hello world").unwrap();

        // Create matching target file
        let target_dir = config_dir.join("target");
        fs::create_dir_all(&target_dir).unwrap();
        let target = target_dir.join("test.txt");
        fs::write(&target, "hello world").unwrap();

        let resolved = make_resolved_profile(
            HashMap::new(),
            FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "files/test.txt".to_string(),
                    target: target.clone(),
                }],
                permissions: HashMap::new(),
            },
        );

        let mut fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let actions = fm.plan(&resolved.merged).unwrap();

        assert!(actions.is_empty());
    }

    #[test]
    fn plan_detects_update_when_content_differs() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        // Create source file
        let files_dir = config_dir.join("files");
        fs::create_dir_all(&files_dir).unwrap();
        fs::write(files_dir.join("test.txt"), "new content").unwrap();

        // Create different target file
        let target_dir = config_dir.join("target");
        fs::create_dir_all(&target_dir).unwrap();
        let target = target_dir.join("test.txt");
        fs::write(&target, "old content").unwrap();

        let resolved = make_resolved_profile(
            HashMap::new(),
            FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "files/test.txt".to_string(),
                    target: target.clone(),
                }],
                permissions: HashMap::new(),
            },
        );

        let mut fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let actions = fm.plan(&resolved.merged).unwrap();

        assert_eq!(actions.len(), 1);
        assert!(
            matches!(&actions[0], FileAction::Update { target: t, diff, .. } if *t == target && !diff.is_empty())
        );
    }

    #[test]
    fn template_rendering_with_variables() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        // Create template file
        let files_dir = config_dir.join("files");
        fs::create_dir_all(&files_dir).unwrap();
        fs::write(
            files_dir.join("config.txt.tera"),
            "editor={{ editor }}\nshell={{ shell }}",
        )
        .unwrap();

        let target = config_dir.join("target").join("config.txt");

        let variables = HashMap::from([
            (
                "editor".to_string(),
                serde_yaml::Value::String("vim".into()),
            ),
            (
                "shell".to_string(),
                serde_yaml::Value::String("/bin/zsh".into()),
            ),
        ]);

        let resolved = make_resolved_profile(
            variables,
            FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "files/config.txt.tera".to_string(),
                    target,
                }],
                permissions: HashMap::new(),
            },
        );

        let mut fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let actions = fm.plan(&resolved.merged).unwrap();

        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], FileAction::Create { .. }));
    }

    #[test]
    fn apply_creates_files() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        // Create source file
        let files_dir = config_dir.join("files");
        fs::create_dir_all(&files_dir).unwrap();
        fs::write(files_dir.join("test.txt"), "hello world").unwrap();

        let target = config_dir.join("output").join("test.txt");

        let resolved = make_resolved_profile(
            HashMap::new(),
            FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "files/test.txt".to_string(),
                    target: target.clone(),
                }],
                permissions: HashMap::new(),
            },
        );

        let mut fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let actions = fm.plan(&resolved.merged).unwrap();

        let printer = crate::output::Printer::new(crate::output::Verbosity::Quiet);
        fm.apply(&actions, &resolved.merged, &printer).unwrap();

        assert!(target.exists());
        assert_eq!(fs::read_to_string(&target).unwrap(), "hello world");
    }

    #[test]
    fn apply_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        // Create source file
        let files_dir = config_dir.join("files");
        fs::create_dir_all(&files_dir).unwrap();
        fs::write(files_dir.join("test.txt"), "hello world").unwrap();

        let target = config_dir.join("output").join("test.txt");

        let resolved = make_resolved_profile(
            HashMap::new(),
            FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "files/test.txt".to_string(),
                    target: target.clone(),
                }],
                permissions: HashMap::new(),
            },
        );

        let printer = crate::output::Printer::new(crate::output::Verbosity::Quiet);

        // First apply
        let mut fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let actions = fm.plan(&resolved.merged).unwrap();
        assert_eq!(actions.len(), 1);
        fm.apply(&actions, &resolved.merged, &printer).unwrap();

        // Second apply — should find nothing to do
        let mut fm2 = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let actions2 = fm2.plan(&resolved.merged).unwrap();
        assert!(actions2.is_empty());
    }

    #[test]
    fn template_custom_functions() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        let files_dir = config_dir.join("files");
        fs::create_dir_all(&files_dir).unwrap();
        fs::write(
            files_dir.join("sys.txt.tera"),
            "os={{ os() }}\narch={{ arch() }}",
        )
        .unwrap();

        let target = config_dir.join("target").join("sys.txt");

        let resolved = make_resolved_profile(
            HashMap::new(),
            FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "files/sys.txt.tera".to_string(),
                    target: target.clone(),
                }],
                permissions: HashMap::new(),
            },
        );

        let printer = crate::output::Printer::new(crate::output::Verbosity::Quiet);

        let mut fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let actions = fm.plan(&resolved.merged).unwrap();
        fm.apply(&actions, &resolved.merged, &printer).unwrap();

        let content = fs::read_to_string(&target).unwrap();
        assert!(content.contains(&format!("os={}", std::env::consts::OS)));
        assert!(content.contains(&format!("arch={}", std::env::consts::ARCH)));
    }

    #[test]
    fn permissions_set_correctly() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        let files_dir = config_dir.join("files");
        fs::create_dir_all(&files_dir).unwrap();
        fs::write(files_dir.join("secret.txt"), "secret data").unwrap();

        let target = config_dir.join("output").join("secret.txt");

        let mut permissions = HashMap::new();
        permissions.insert(target.display().to_string(), "600".to_string());

        let resolved = make_resolved_profile(
            HashMap::new(),
            FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "files/secret.txt".to_string(),
                    target: target.clone(),
                }],
                permissions,
            },
        );

        let printer = crate::output::Printer::new(crate::output::Verbosity::Quiet);

        let mut fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let actions = fm.plan(&resolved.merged).unwrap();
        fm.apply(&actions, &resolved.merged, &printer).unwrap();

        let metadata = fs::metadata(&target).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn add_file_copies_to_source_dir() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        // Create a file to track
        let original = config_dir.join("existing_config.toml");
        fs::write(&original, "key = 'value'").unwrap();

        let resolved = make_resolved_profile(HashMap::new(), FilesSpec::default());
        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();

        let spec = fm.add_file(&original, "test").unwrap();

        assert!(spec.source.starts_with("files/test/"));
        assert_eq!(spec.target, original);

        // Verify the file was copied
        let copied = config_dir.join(&spec.source);
        assert!(copied.exists());
        assert_eq!(fs::read_to_string(&copied).unwrap(), "key = 'value'");
    }

    #[test]
    fn source_not_found_error() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        let target = config_dir.join("target").join("test.txt");

        let resolved = make_resolved_profile(
            HashMap::new(),
            FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "nonexistent/file.txt".to_string(),
                    target,
                }],
                permissions: HashMap::new(),
            },
        );

        let mut fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let result = fm.plan(&resolved.merged);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn template_error_includes_path() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        let files_dir = config_dir.join("files");
        fs::create_dir_all(&files_dir).unwrap();
        // Invalid template syntax
        fs::write(files_dir.join("bad.txt.tera"), "{{ invalid %}").unwrap();

        let target = config_dir.join("target").join("bad.txt");

        let resolved = make_resolved_profile(
            HashMap::new(),
            FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "files/bad.txt.tera".to_string(),
                    target,
                }],
                permissions: HashMap::new(),
            },
        );

        let mut fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let result = fm.plan(&resolved.merged);
        assert!(result.is_err());
        let err_str = result.unwrap_err().to_string();
        assert!(err_str.contains("template"));
    }

    #[test]
    fn scan_source_builds_file_tree() {
        let dir = tempfile::tempdir().unwrap();
        let source_dir = dir.path().join("source");
        fs::create_dir_all(source_dir.join("sub")).unwrap();
        fs::write(source_dir.join("file1.txt"), "content1").unwrap();
        fs::write(source_dir.join("sub").join("file2.txt"), "content2").unwrap();

        let resolved = make_resolved_profile(HashMap::new(), FilesSpec::default());
        let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

        use crate::providers::FileManager as _;
        let layers = vec![FileLayer {
            source_dir,
            origin_source: "local".to_string(),
            priority: 1000,
        }];

        let tree = fm.scan_source(&layers).unwrap();
        assert_eq!(tree.files.len(), 2);
    }
}
