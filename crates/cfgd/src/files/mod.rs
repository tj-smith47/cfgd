use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use similar::TextDiff;
use tera::{Context, Tera};

use cfgd_core::config::{EnvVar, FileStrategy, ManagedFileSpec, MergedProfile, ResolvedProfile};
use cfgd_core::errors::{FileError, Result};
use cfgd_core::expand_tilde;
use cfgd_core::output::Printer;
use cfgd_core::providers::{FileAction, FileDiff, FileDiffKind, FileEntry, FileLayer, FileTree};

/// Check if a path is a Tera template file (has .tera extension).
pub(crate) fn is_tera_template(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("tera")
}

/// Concrete FileManager implementation for cfgd.
/// Manages files declared in profiles: copy, template, diff, permissions.
pub struct CfgdFileManager {
    tera: Mutex<Tera>,
    context: Context,
    config_dir: PathBuf,
    secret_backend: Option<Box<dyn cfgd_core::providers::SecretBackend>>,
    secret_providers: Vec<Box<dyn cfgd_core::providers::SecretProvider>>,
    /// Per-source env contexts for template sandboxing.
    /// Source templates can only access their own env vars + system facts.
    source_contexts: HashMap<String, Context>,
    /// Global default file deployment strategy.
    global_strategy: FileStrategy,
}

impl CfgdFileManager {
    /// Create a new file manager.
    /// `config_dir` is the directory containing cfgd.yaml (used to resolve relative source paths).
    /// `resolved` is the fully resolved profile with merged env vars.
    pub fn new(config_dir: &Path, resolved: &ResolvedProfile) -> Result<Self> {
        let mut tera = Tera::default();
        tera.autoescape_on(vec![]);

        let mut context = Context::new();

        // Flat profile env vars
        for ev in &resolved.merged.env {
            context.insert(&ev.name, &ev.value);
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

        // Empty sources map for templates — populated by set_source_env() when
        // multi-source composition is active.
        let sources: HashMap<String, HashMap<String, String>> = HashMap::new();
        context.insert("sources", &sources);

        Ok(Self {
            tera: Mutex::new(tera),
            context,
            config_dir: config_dir.to_path_buf(),
            secret_backend: None,
            secret_providers: Vec::new(),
            source_contexts: HashMap::new(),
            global_strategy: FileStrategy::default(),
        })
    }

    /// Set per-source env contexts for template sandboxing.
    /// Source templates will only have access to their source's env vars
    /// and system facts — not the subscriber's personal env vars.
    pub fn set_source_env(&mut self, source_env: &HashMap<String, Vec<EnvVar>>) {
        for (source_name, env) in source_env {
            let mut ctx = Context::new();
            for ev in env {
                ctx.insert(&ev.name, &ev.value);
            }
            // System facts are always available
            ctx.insert("__os", &std::env::consts::OS);
            ctx.insert("__arch", &std::env::consts::ARCH);
            ctx.insert(
                "__hostname",
                &hostname::get()
                    .map(|h| h.to_string_lossy().to_string())
                    .unwrap_or_default(),
            );
            self.source_contexts.insert(source_name.clone(), ctx);
        }
    }

    /// Set the global file deployment strategy.
    pub fn set_global_strategy(&mut self, strategy: FileStrategy) {
        self.global_strategy = strategy;
    }

    /// Set secret backend and providers for resolving `${secret:ref}` in templates.
    pub fn set_secret_providers(
        &mut self,
        backend: Option<Box<dyn cfgd_core::providers::SecretBackend>>,
        providers: Vec<Box<dyn cfgd_core::providers::SecretProvider>>,
    ) {
        self.secret_backend = backend;
        self.secret_providers = providers;
    }

    /// Resolve the effective strategy for a managed file.
    /// Template files always use Copy (can't symlink unrendered templates).
    fn effective_strategy(&self, source: &Path, per_file: Option<FileStrategy>) -> FileStrategy {
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
                    FileStrategy::Hardlink => is_same_inode(&source_path, &target_path),
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
    pub fn diff(&self, profile: &MergedProfile, printer: &Printer) -> Result<()> {
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

        Ok(())
    }

    /// Render a template and return the content for display (e.g., in plan diffs).
    pub fn render_template_for_display(&self, path: &Path) -> Result<String> {
        self.render_template(path, None)
    }

    /// Render a .tera template file with profile env vars and system facts.
    /// If `source_origin` is Some, uses a restricted context with only that
    /// source's env vars — source templates cannot access local env vars.
    fn render_template(&self, path: &Path, source_origin: Option<&str>) -> Result<String> {
        let template_content = fs::read_to_string(path).map_err(|e| FileError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;

        let template_name = path.display().to_string();
        let mut tera = self.tera.lock().map_err(|_| FileError::TemplateError {
            path: path.to_path_buf(),
            message: "tera mutex poisoned".to_string(),
        })?;
        tera.add_raw_template(&template_name, &template_content)
            .map_err(|e| FileError::TemplateError {
                path: path.to_path_buf(),
                message: format_tera_error(&e),
            })?;

        // Register custom functions
        register_custom_functions(&mut tera);

        // Use source-restricted context if this file came from a source
        let ctx = match source_origin {
            Some(source_name) => self
                .source_contexts
                .get(source_name)
                .unwrap_or(&self.context),
            None => &self.context,
        };

        tera.render(&template_name, ctx).map_err(|e| {
            let msg = format_tera_error(&e);
            // If a source template references an undefined variable, it means
            // it tried to access a local variable that isn't in its sandbox.
            if source_origin.is_some() && msg.contains("not found in context") {
                let var_name = msg
                    .split("Variable `")
                    .nth(1)
                    .and_then(|s| s.split('`').next())
                    .unwrap_or("unknown");
                return cfgd_core::errors::CompositionError::TemplateSandboxViolation {
                    source_name: source_origin.unwrap_or("unknown").to_string(),
                    variable: var_name.to_string(),
                }
                .into();
            }
            FileError::TemplateError {
                path: path.to_path_buf(),
                message: msg,
            }
            .into()
        })
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

        Ok(None)
    }

    /// Resolve a source path relative to the config directory.
    fn resolve_source_path(&self, source: &str) -> std::result::Result<PathBuf, FileError> {
        let path = PathBuf::from(source);
        if path.is_absolute() {
            Ok(path)
        } else {
            let resolved = self.config_dir.join(source);
            // Validate the resolved path doesn't escape the config directory via ../
            if cfgd_core::validate_no_traversal(&resolved).is_err() {
                return Err(FileError::PathTraversal {
                    path: resolved,
                    root: self.config_dir.clone(),
                });
            }
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
}

/// Implement the FileManager trait for CfgdFileManager.
impl cfgd_core::providers::FileManager for CfgdFileManager {
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

        pb.finish_and_clear();
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
                permissions: Some(metadata.permissions().mode() & 0o777),
                is_template: is_tera_template(&path),
                source_path: path,
                origin_source: origin.to_string(),
            },
        );
    }

    Ok(())
}

/// Check if two paths point to the same inode (hard link check).
fn is_same_inode(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (fs::metadata(a), fs::metadata(b)) {
        (Ok(ma), Ok(mb)) => ma.ino() == mb.ino() && ma.dev() == mb.dev(),
        _ => false,
    }
}

/// Ensure the target's parent directory exists and the target is writable.
fn ensure_target_writable(target: &Path) -> Result<()> {
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

    use cfgd_core::config::{
        EnvVar, FilesSpec, LayerPolicy, ManagedFileSpec, MergedProfile, ProfileLayer, ProfileSpec,
        ResolvedProfile,
    };
    use cfgd_core::providers::FileManager as _;

    fn make_resolved_profile(env: Vec<EnvVar>, files: FilesSpec) -> ResolvedProfile {
        ResolvedProfile {
            layers: vec![ProfileLayer {
                source: "local".to_string(),
                profile_name: "test".to_string(),
                priority: 1000,
                policy: LayerPolicy::Local,
                spec: ProfileSpec::default(),
            }],
            merged: MergedProfile {
                env,
                files,
                ..Default::default()
            },
        }
    }

    #[test]
    fn expand_tilde_with_home() {
        // SAFETY: This test runs single-threaded; no concurrent env access.
        unsafe { std::env::set_var("HOME", "/home/testuser") };
        let result = expand_tilde(Path::new("~/.config/test"));
        assert_eq!(result, PathBuf::from("/home/testuser/.config/test"));
    }

    #[test]
    fn expand_tilde_absolute_path_unchanged() {
        let result = expand_tilde(Path::new("/etc/config"));
        assert_eq!(result, PathBuf::from("/etc/config"));
    }

    #[test]
    fn sha256_hex_deterministic() {
        let h1 = cfgd_core::sha256_hex(b"hello world");
        let h2 = cfgd_core::sha256_hex(b"hello world");
        assert_eq!(h1, h2);
        assert_ne!(h1, cfgd_core::sha256_hex(b"different"));
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
            vec![],
            FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "files/test.txt".to_string(),
                    target: target.clone(),
                    strategy: Some(FileStrategy::Copy),
                    private: false,
                    origin: None,
                }],
                permissions: HashMap::new(),
            },
        );

        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
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
            vec![],
            FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "files/test.txt".to_string(),
                    target: target.clone(),
                    strategy: Some(FileStrategy::Copy),
                    private: false,
                    origin: None,
                }],
                permissions: HashMap::new(),
            },
        );

        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
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
            vec![],
            FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "files/test.txt".to_string(),
                    target: target.clone(),
                    strategy: Some(FileStrategy::Copy),
                    private: false,
                    origin: None,
                }],
                permissions: HashMap::new(),
            },
        );

        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let actions = fm.plan(&resolved.merged).unwrap();

        assert_eq!(actions.len(), 1);
        assert!(
            matches!(&actions[0], FileAction::Update { target: t, diff, .. } if *t == target && !diff.is_empty())
        );
    }

    #[test]
    fn template_rendering_with_env() {
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

        let env = vec![
            EnvVar {
                name: "editor".into(),
                value: "vim".into(),
            },
            EnvVar {
                name: "shell".into(),
                value: "/bin/zsh".into(),
            },
        ];

        let resolved = make_resolved_profile(
            env,
            FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "files/config.txt.tera".to_string(),
                    target,
                    strategy: Some(FileStrategy::Copy),
                    private: false,
                    origin: None,
                }],
                permissions: HashMap::new(),
            },
        );

        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
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
            vec![],
            FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "files/test.txt".to_string(),
                    target: target.clone(),
                    strategy: Some(FileStrategy::Copy),
                    private: false,
                    origin: None,
                }],
                permissions: HashMap::new(),
            },
        );

        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let actions = fm.plan(&resolved.merged).unwrap();

        let printer = cfgd_core::output::Printer::new(cfgd_core::output::Verbosity::Quiet);
        fm.apply(&actions, &printer).unwrap();

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
            vec![],
            FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "files/test.txt".to_string(),
                    target: target.clone(),
                    strategy: Some(FileStrategy::Copy),
                    private: false,
                    origin: None,
                }],
                permissions: HashMap::new(),
            },
        );

        let printer = cfgd_core::output::Printer::new(cfgd_core::output::Verbosity::Quiet);

        // First apply
        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let actions = fm.plan(&resolved.merged).unwrap();
        assert_eq!(actions.len(), 1);
        fm.apply(&actions, &printer).unwrap();

        // Second apply — should find nothing to do
        let fm2 = CfgdFileManager::new(config_dir, &resolved).unwrap();
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
            vec![],
            FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "files/sys.txt.tera".to_string(),
                    target: target.clone(),
                    strategy: Some(FileStrategy::Copy),
                    private: false,
                    origin: None,
                }],
                permissions: HashMap::new(),
            },
        );

        let printer = cfgd_core::output::Printer::new(cfgd_core::output::Verbosity::Quiet);

        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let actions = fm.plan(&resolved.merged).unwrap();
        fm.apply(&actions, &printer).unwrap();

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
            vec![],
            FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "files/secret.txt".to_string(),
                    target: target.clone(),
                    strategy: Some(FileStrategy::Copy),
                    private: false,
                    origin: None,
                }],
                permissions,
            },
        );

        let printer = cfgd_core::output::Printer::new(cfgd_core::output::Verbosity::Quiet);

        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let actions = fm.plan(&resolved.merged).unwrap();
        fm.apply(&actions, &printer).unwrap();

        let metadata = fs::metadata(&target).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn source_not_found_error() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        let target = config_dir.join("target").join("test.txt");

        let resolved = make_resolved_profile(
            vec![],
            FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "nonexistent/file.txt".to_string(),
                    target,
                    strategy: Some(FileStrategy::Copy),
                    private: false,
                    origin: None,
                }],
                permissions: HashMap::new(),
            },
        );

        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
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
            vec![],
            FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "files/bad.txt.tera".to_string(),
                    target,
                    strategy: Some(FileStrategy::Copy),
                    private: false,
                    origin: None,
                }],
                permissions: HashMap::new(),
            },
        );

        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
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

        let resolved = make_resolved_profile(vec![], FilesSpec::default());
        let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

        use cfgd_core::providers::FileManager as _;
        let layers = vec![FileLayer {
            source_dir,
            origin_source: "local".to_string(),
            priority: 1000,
        }];

        let tree = fm.scan_source(&layers).unwrap();
        assert_eq!(tree.files.len(), 2);
    }

    #[test]
    fn source_template_cannot_access_local_env() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        // Create a source template that tries to use a local variable
        let files_dir = config_dir.join("files");
        fs::create_dir_all(&files_dir).unwrap();
        fs::write(
            files_dir.join("team.txt.tera"),
            "team={{ team_name }}\nlocal={{ personal_var }}",
        )
        .unwrap();

        let target = config_dir.join("target").join("team.txt");

        // Local profile has personal_var but NOT team_name
        let local_env = vec![EnvVar {
            name: "personal_var".into(),
            value: "my-secret".into(),
        }];

        let resolved = make_resolved_profile(
            local_env,
            FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "files/team.txt.tera".to_string(),
                    target,
                    strategy: Some(FileStrategy::Copy),
                    private: false,
                    origin: Some("acme-corp".to_string()),
                }],
                permissions: HashMap::new(),
            },
        );

        let mut fm = CfgdFileManager::new(config_dir, &resolved).unwrap();

        // Set source env — acme-corp only has team_name, NOT personal_var
        let mut source_vars = HashMap::new();
        source_vars.insert(
            "acme-corp".to_string(),
            vec![EnvVar {
                name: "team_name".into(),
                value: "Platform".into(),
            }],
        );
        fm.set_source_env(&source_vars);

        // Planning should fail because the template references personal_var
        // which is NOT in the source's sandboxed context
        let result = fm.plan(&resolved.merged);
        assert!(
            result.is_err(),
            "Source template should not access local env vars"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("personal_var"),
            "Error should mention the inaccessible variable: {err}"
        );
    }

    #[test]
    fn source_template_can_access_own_env() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        // Create a source template using only source-provided env vars
        let files_dir = config_dir.join("files");
        fs::create_dir_all(&files_dir).unwrap();
        fs::write(files_dir.join("team.txt.tera"), "team={{ team_name }}").unwrap();

        let target = config_dir.join("target").join("team.txt");

        let resolved = make_resolved_profile(
            vec![],
            FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "files/team.txt.tera".to_string(),
                    target: target.clone(),
                    strategy: Some(FileStrategy::Copy),
                    private: false,
                    origin: Some("acme-corp".to_string()),
                }],
                permissions: HashMap::new(),
            },
        );

        let mut fm = CfgdFileManager::new(config_dir, &resolved).unwrap();

        // Set source env
        let mut source_vars = HashMap::new();
        source_vars.insert(
            "acme-corp".to_string(),
            vec![EnvVar {
                name: "team_name".into(),
                value: "Platform".into(),
            }],
        );
        fm.set_source_env(&source_vars);

        // Should succeed — template only uses its own env vars
        let actions = fm.plan(&resolved.merged).unwrap();
        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], FileAction::Create { .. }));
    }

    #[test]
    fn source_template_can_access_system_facts() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        // Create a source template using system facts
        let files_dir = config_dir.join("files");
        fs::create_dir_all(&files_dir).unwrap();
        fs::write(
            files_dir.join("info.txt.tera"),
            "os={{ __os }}\narch={{ __arch }}",
        )
        .unwrap();

        let target = config_dir.join("target").join("info.txt");

        let resolved = make_resolved_profile(
            vec![],
            FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "files/info.txt.tera".to_string(),
                    target: target.clone(),
                    strategy: Some(FileStrategy::Copy),
                    private: false,
                    origin: Some("acme-corp".to_string()),
                }],
                permissions: HashMap::new(),
            },
        );

        let mut fm = CfgdFileManager::new(config_dir, &resolved).unwrap();

        // Set source env (empty — but system facts should still be available)
        let mut source_vars: HashMap<String, Vec<EnvVar>> = HashMap::new();
        source_vars.insert("acme-corp".to_string(), vec![]);
        fm.set_source_env(&source_vars);

        // Should succeed — system facts are always available
        let actions = fm.plan(&resolved.merged).unwrap();
        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], FileAction::Create { .. }));
    }

    #[test]
    fn symlink_strategy_creates_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        let files_dir = config_dir.join("files");
        fs::create_dir_all(&files_dir).unwrap();
        fs::write(files_dir.join("test.txt"), "symlink content").unwrap();

        let target = config_dir.join("output").join("test.txt");

        let resolved = make_resolved_profile(
            vec![],
            FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "files/test.txt".to_string(),
                    target: target.clone(),
                    strategy: Some(FileStrategy::Symlink),
                    private: false,
                    origin: None,
                }],
                permissions: HashMap::new(),
            },
        );

        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let actions = fm.plan(&resolved.merged).unwrap();
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0],
            FileAction::Create {
                strategy: FileStrategy::Symlink,
                ..
            }
        ));

        let printer = cfgd_core::output::Printer::new(cfgd_core::output::Verbosity::Quiet);
        fm.apply(&actions, &printer).unwrap();

        assert!(target.is_symlink());
        let link_target = fs::read_link(&target).unwrap();
        assert_eq!(link_target, files_dir.join("test.txt"));
        assert_eq!(fs::read_to_string(&target).unwrap(), "symlink content");
    }

    #[test]
    fn symlink_strategy_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        let files_dir = config_dir.join("files");
        fs::create_dir_all(&files_dir).unwrap();
        fs::write(files_dir.join("test.txt"), "content").unwrap();

        let target = config_dir.join("output").join("test.txt");

        let resolved = make_resolved_profile(
            vec![],
            FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "files/test.txt".to_string(),
                    target: target.clone(),
                    strategy: Some(FileStrategy::Symlink),
                    private: false,
                    origin: None,
                }],
                permissions: HashMap::new(),
            },
        );

        let printer = cfgd_core::output::Printer::new(cfgd_core::output::Verbosity::Quiet);

        // First apply
        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let actions = fm.plan(&resolved.merged).unwrap();
        assert_eq!(actions.len(), 1);
        fm.apply(&actions, &printer).unwrap();

        // Second plan — symlink already points to correct source, no actions needed
        let fm2 = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let actions2 = fm2.plan(&resolved.merged).unwrap();
        assert!(
            actions2.is_empty(),
            "expected no actions, got {:?}",
            actions2
        );
    }

    #[test]
    fn hardlink_strategy_creates_hardlink() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        let files_dir = config_dir.join("files");
        fs::create_dir_all(&files_dir).unwrap();
        fs::write(files_dir.join("test.txt"), "hardlink content").unwrap();

        let target = config_dir.join("output").join("test.txt");

        let resolved = make_resolved_profile(
            vec![],
            FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "files/test.txt".to_string(),
                    target: target.clone(),
                    strategy: Some(FileStrategy::Hardlink),
                    private: false,
                    origin: None,
                }],
                permissions: HashMap::new(),
            },
        );

        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let actions = fm.plan(&resolved.merged).unwrap();
        assert_eq!(actions.len(), 1);

        let printer = cfgd_core::output::Printer::new(cfgd_core::output::Verbosity::Quiet);
        fm.apply(&actions, &printer).unwrap();

        assert!(target.exists());
        assert!(is_same_inode(&files_dir.join("test.txt"), &target));
        assert_eq!(fs::read_to_string(&target).unwrap(), "hardlink content");
    }

    #[test]
    fn template_auto_upgrades_to_copy() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        let files_dir = config_dir.join("files");
        fs::create_dir_all(&files_dir).unwrap();
        fs::write(files_dir.join("config.txt.tera"), "val={{ val }}").unwrap();

        let target = config_dir.join("output").join("config.txt");

        let env = vec![EnvVar {
            name: "val".into(),
            value: "hello".into(),
        }];

        let resolved = make_resolved_profile(
            env,
            FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "files/config.txt.tera".to_string(),
                    target: target.clone(),
                    // Explicitly set Symlink — but template should force Copy
                    strategy: Some(FileStrategy::Symlink),
                    private: false,
                    origin: None,
                }],
                permissions: HashMap::new(),
            },
        );

        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let actions = fm.plan(&resolved.merged).unwrap();
        assert_eq!(actions.len(), 1);
        // Strategy should be Copy despite Symlink being requested
        assert!(matches!(
            &actions[0],
            FileAction::Create {
                strategy: FileStrategy::Copy,
                ..
            }
        ));

        let printer = cfgd_core::output::Printer::new(cfgd_core::output::Verbosity::Quiet);
        fm.apply(&actions, &printer).unwrap();

        // Should be a regular file, not a symlink
        assert!(!target.is_symlink());
        assert_eq!(fs::read_to_string(&target).unwrap(), "val=hello");
    }

    #[test]
    fn global_strategy_applies_when_no_per_file_override() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        let files_dir = config_dir.join("files");
        fs::create_dir_all(&files_dir).unwrap();
        fs::write(files_dir.join("test.txt"), "content").unwrap();

        let target = config_dir.join("output").join("test.txt");

        let resolved = make_resolved_profile(
            vec![],
            FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "files/test.txt".to_string(),
                    target: target.clone(),
                    strategy: None, // No per-file override
                    private: false,
                    origin: None,
                }],
                permissions: HashMap::new(),
            },
        );

        let mut fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        fm.set_global_strategy(FileStrategy::Copy);

        let actions = fm.plan(&resolved.merged).unwrap();
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0],
            FileAction::Create {
                strategy: FileStrategy::Copy,
                ..
            }
        ));
    }

    #[test]
    fn private_file_skipped_when_source_missing() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        let target = config_dir.join("target").join("secret.txt");

        let resolved = make_resolved_profile(
            vec![],
            FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "files/nonexistent.txt".to_string(),
                    target: target.clone(),
                    strategy: Some(FileStrategy::Copy),
                    private: true,
                    origin: None,
                }],
                permissions: HashMap::new(),
            },
        );

        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let actions = fm.plan(&resolved.merged).unwrap();
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0],
            FileAction::Skip { reason, .. } if reason.contains("private")
        ));
    }

    #[test]
    fn non_private_file_errors_when_source_missing() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        let target = config_dir.join("target").join("test.txt");

        let resolved = make_resolved_profile(
            vec![],
            FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "files/nonexistent.txt".to_string(),
                    target,
                    strategy: Some(FileStrategy::Copy),
                    private: false,
                    origin: None,
                }],
                permissions: HashMap::new(),
            },
        );

        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let result = fm.plan(&resolved.merged);
        assert!(result.is_err());
    }

    // --- is_tera_template ---

    #[test]
    fn is_tera_template_true() {
        assert!(is_tera_template(Path::new("config.txt.tera")));
    }

    #[test]
    fn is_tera_template_false() {
        assert!(!is_tera_template(Path::new("config.txt")));
        assert!(!is_tera_template(Path::new("noext")));
    }

    // --- diff ---

    #[test]
    fn diff_no_changes_prints_success() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        let files_dir = config_dir.join("files");
        fs::create_dir_all(&files_dir).unwrap();
        fs::write(files_dir.join("test.txt"), "same content").unwrap();

        let target_dir = config_dir.join("target");
        fs::create_dir_all(&target_dir).unwrap();
        let target = target_dir.join("test.txt");
        fs::write(&target, "same content").unwrap();

        let resolved = make_resolved_profile(
            vec![],
            FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "files/test.txt".to_string(),
                    target,
                    strategy: Some(FileStrategy::Copy),
                    private: false,
                    origin: None,
                }],
                permissions: HashMap::new(),
            },
        );

        let printer = cfgd_core::output::Printer::new(cfgd_core::output::Verbosity::Quiet);
        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        assert!(fm.diff(&resolved.merged, &printer).is_ok());
    }

    #[test]
    fn diff_detects_content_difference() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        let files_dir = config_dir.join("files");
        fs::create_dir_all(&files_dir).unwrap();
        fs::write(files_dir.join("test.txt"), "new").unwrap();

        let target_dir = config_dir.join("target");
        fs::create_dir_all(&target_dir).unwrap();
        let target = target_dir.join("test.txt");
        fs::write(&target, "old").unwrap();

        let resolved = make_resolved_profile(
            vec![],
            FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "files/test.txt".to_string(),
                    target,
                    strategy: Some(FileStrategy::Copy),
                    private: false,
                    origin: None,
                }],
                permissions: HashMap::new(),
            },
        );

        let printer = cfgd_core::output::Printer::new(cfgd_core::output::Verbosity::Quiet);
        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        // Should succeed (diff is displayed, not an error)
        assert!(fm.diff(&resolved.merged, &printer).is_ok());
    }

    #[test]
    fn diff_new_file_shown() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        let files_dir = config_dir.join("files");
        fs::create_dir_all(&files_dir).unwrap();
        fs::write(files_dir.join("new.txt"), "brand new").unwrap();

        let target = config_dir.join("nonexistent").join("new.txt");

        let resolved = make_resolved_profile(
            vec![],
            FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "files/new.txt".to_string(),
                    target,
                    strategy: Some(FileStrategy::Copy),
                    private: false,
                    origin: None,
                }],
                permissions: HashMap::new(),
            },
        );

        let printer = cfgd_core::output::Printer::new(cfgd_core::output::Verbosity::Quiet);
        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        assert!(fm.diff(&resolved.merged, &printer).is_ok());
    }

    // --- render_template_for_display ---

    #[test]
    fn render_template_for_display_basic() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        let files_dir = config_dir.join("files");
        fs::create_dir_all(&files_dir).unwrap();
        let tpl = files_dir.join("greeting.txt.tera");
        fs::write(&tpl, "Hello {{ name }}!").unwrap();

        let env = vec![EnvVar {
            name: "name".into(),
            value: "world".into(),
        }];

        let resolved = make_resolved_profile(env, FilesSpec::default());
        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();

        let rendered = fm.render_template_for_display(&tpl).unwrap();
        assert_eq!(rendered, "Hello world!");
    }

    // --- check_permissions ---

    #[test]
    fn check_permissions_drift_detected() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        let target = config_dir.join("secret.txt");
        fs::write(&target, "data").unwrap();
        // Set permissive permissions
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();

        let mut permissions = HashMap::new();
        permissions.insert(target.display().to_string(), "600".to_string());

        let managed = ManagedFileSpec {
            source: "files/secret.txt".to_string(),
            target: target.clone(),
            strategy: Some(FileStrategy::Copy),
            private: false,
            origin: None,
        };

        let resolved = make_resolved_profile(
            vec![],
            FilesSpec {
                managed: vec![managed.clone()],
                permissions,
            },
        );

        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let action = fm
            .check_permissions(&target, &managed, &resolved.merged)
            .unwrap();
        assert!(action.is_some());
        assert!(matches!(
            action.unwrap(),
            FileAction::SetPermissions { mode: 0o600, .. }
        ));
    }

    #[test]
    fn check_permissions_no_drift() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        let target = config_dir.join("secret.txt");
        fs::write(&target, "data").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();

        let mut permissions = HashMap::new();
        permissions.insert(target.display().to_string(), "600".to_string());

        let managed = ManagedFileSpec {
            source: "files/secret.txt".to_string(),
            target: target.clone(),
            strategy: Some(FileStrategy::Copy),
            private: false,
            origin: None,
        };

        let resolved = make_resolved_profile(
            vec![],
            FilesSpec {
                managed: vec![managed.clone()],
                permissions,
            },
        );

        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let action = fm
            .check_permissions(&target, &managed, &resolved.merged)
            .unwrap();
        assert!(action.is_none());
    }

    // --- set_permissions ---

    #[test]
    fn set_permissions_changes_mode() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "data").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();

        set_permissions(&file, 0o600).unwrap();

        let metadata = fs::metadata(&file).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    }

    // --- ensure_target_writable ---

    #[test]
    fn ensure_target_writable_creates_parent() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("sub").join("deep").join("file.txt");
        ensure_target_writable(&target).unwrap();
        assert!(target.parent().unwrap().exists());
    }

    // --- format_tera_error ---

    #[test]
    fn format_tera_error_basic() {
        // Create a tera error by trying to render invalid template
        let mut tera = Tera::default();
        let err = tera.add_raw_template("bad", "{{ invalid %}").unwrap_err();
        let formatted = format_tera_error(&err);
        assert!(!formatted.is_empty());
    }

    // --- multiple files in plan ---

    #[test]
    fn plan_multiple_files() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        let files_dir = config_dir.join("files");
        fs::create_dir_all(&files_dir).unwrap();
        fs::write(files_dir.join("a.txt"), "aaa").unwrap();
        fs::write(files_dir.join("b.txt"), "bbb").unwrap();

        let target_a = config_dir.join("out").join("a.txt");
        let target_b = config_dir.join("out").join("b.txt");

        let resolved = make_resolved_profile(
            vec![],
            FilesSpec {
                managed: vec![
                    ManagedFileSpec {
                        source: "files/a.txt".to_string(),
                        target: target_a,
                        strategy: Some(FileStrategy::Copy),
                        private: false,
                        origin: None,
                    },
                    ManagedFileSpec {
                        source: "files/b.txt".to_string(),
                        target: target_b,
                        strategy: Some(FileStrategy::Copy),
                        private: false,
                        origin: None,
                    },
                ],
                permissions: HashMap::new(),
            },
        );

        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let actions = fm.plan(&resolved.merged).unwrap();
        assert_eq!(actions.len(), 2);
    }

    // --- apply update overwrites ---

    #[test]
    fn apply_update_overwrites_target() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();

        let files_dir = config_dir.join("files");
        fs::create_dir_all(&files_dir).unwrap();
        fs::write(files_dir.join("test.txt"), "updated content").unwrap();

        let target_dir = config_dir.join("output");
        fs::create_dir_all(&target_dir).unwrap();
        let target = target_dir.join("test.txt");
        fs::write(&target, "old content").unwrap();

        let resolved = make_resolved_profile(
            vec![],
            FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "files/test.txt".to_string(),
                    target: target.clone(),
                    strategy: Some(FileStrategy::Copy),
                    private: false,
                    origin: None,
                }],
                permissions: HashMap::new(),
            },
        );

        let printer = cfgd_core::output::Printer::new(cfgd_core::output::Verbosity::Quiet);
        let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
        let actions = fm.plan(&resolved.merged).unwrap();
        assert_eq!(actions.len(), 1);
        fm.apply(&actions, &printer).unwrap();

        assert_eq!(fs::read_to_string(&target).unwrap(), "updated content");
    }
}
