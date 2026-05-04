use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tera::{Context, Tera};

use cfgd_core::config::{EnvVar, FileStrategy, ResolvedProfile};
use cfgd_core::errors::{FileError, Result};

mod apply;
mod plan;
mod template;

#[cfg(test)]
mod tests;

pub(crate) use template::is_tera_template;

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

        template::insert_system_facts(&mut context);

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
            template::insert_system_facts(&mut ctx);
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
}

/// Check whether a file is encrypted with the given backend.
///
/// - `"sops"`: parses the file as YAML or JSON and checks for a top-level `sops` key
///   that contains both `mac` and `lastmodified` sub-keys, which SOPS always writes.
///   This avoids false positives from files that merely mention "sops" in comments.
/// - `"age"`: checks whether the file begins with the `age-encryption.org` magic header.
/// - Any other backend: returns `FileError::UnknownEncryptionBackend`.
pub(crate) fn is_file_encrypted(
    path: &Path,
    backend: &str,
) -> std::result::Result<bool, FileError> {
    cfgd_core::is_file_encrypted(path, backend)
}
