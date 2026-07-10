use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::atomic_write_str;
use crate::errors::{CfgdError, GenerateError};
use crate::generate::SchemaKind;
use crate::generate::validate::validate_yaml;

/// Tracks state for a generate session.
#[derive(Debug)]
pub struct GenerateSession {
    repo_root: PathBuf,
    // The cfgd binary crate's version, threaded in at construction: SchemaStore
    // vendored filenames are stamped with the cfgd crate's version (schemas
    // declare `crate: cfgd`), and the workspace releases crates independently,
    // so cfgd-core's own CARGO_PKG_VERSION would 404 after any drift.
    schema_version: String,
    generated: HashMap<String, GeneratedItem>,
}

#[derive(Debug, Clone)]
pub struct GeneratedItem {
    pub kind: SchemaKind,
    pub name: String,
    pub path: PathBuf,
}

impl GenerateSession {
    /// `schema_version` is the calling binary crate's version (the cfgd CLI
    /// passes its own `env!("CARGO_PKG_VERSION")`); it stamps the SchemaStore
    /// URL in generated documents' modelines.
    pub fn new(repo_root: PathBuf, schema_version: impl Into<String>) -> Self {
        Self {
            repo_root,
            schema_version: schema_version.into(),
            generated: HashMap::new(),
        }
    }

    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    pub fn write_module_yaml(&mut self, name: &str, content: &str) -> Result<PathBuf, CfgdError> {
        let result = validate_yaml(content, SchemaKind::Module);
        if !result.valid {
            return Err(GenerateError::ValidationFailed {
                message: format!("Invalid module YAML: {}", result.errors.join("; ")),
            }
            .into());
        }
        let dir = self.repo_root.join("modules").join(name);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("module.yaml");
        let content = crate::config::with_schema_modeline(
            crate::config::SchemaDocKind::Module,
            &self.schema_version,
            content,
        );
        atomic_write_str(&path, &content)?;
        let key = format!("module:{}", name);
        self.generated.insert(
            key,
            GeneratedItem {
                kind: SchemaKind::Module,
                name: name.to_string(),
                path: path.clone(),
            },
        );
        Ok(path)
    }

    pub fn write_profile_yaml(&mut self, name: &str, content: &str) -> Result<PathBuf, CfgdError> {
        let result = validate_yaml(content, SchemaKind::Profile);
        if !result.valid {
            return Err(GenerateError::ValidationFailed {
                message: format!("Invalid profile YAML: {}", result.errors.join("; ")),
            }
            .into());
        }
        // Overwrite an existing manifest in whichever form it already has —
        // writing canonical unconditionally beside a legacy flat file would
        // manufacture an AmbiguousProfile state. Ambiguity propagates.
        let profiles_dir = self.repo_root.join("profiles");
        let path = match crate::config::find_profile_path(&profiles_dir, name) {
            Ok(existing) => existing,
            Err(crate::errors::ConfigError::ProfileNotFound { .. }) => {
                let canonical = crate::config::canonical_profile_path(&profiles_dir, name);
                if let Some(parent) = canonical.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                canonical
            }
            Err(e) => return Err(e.into()),
        };
        let content = crate::config::with_schema_modeline(
            crate::config::SchemaDocKind::Profile,
            &self.schema_version,
            content,
        );
        atomic_write_str(&path, &content)?;
        let key = format!("profile:{}", name);
        self.generated.insert(
            key,
            GeneratedItem {
                kind: SchemaKind::Profile,
                name: name.to_string(),
                path: path.clone(),
            },
        );
        Ok(path)
    }

    pub fn list_generated(&self) -> Vec<&GeneratedItem> {
        self.generated.values().collect()
    }

    pub fn get_existing_modules(&self) -> Result<Vec<String>, CfgdError> {
        let modules_dir = self.repo_root.join("modules");
        if !modules_dir.exists() {
            return Ok(vec![]);
        }
        let mut names = vec![];
        for entry in std::fs::read_dir(&modules_dir)? {
            let entry = entry?;
            if entry.path().is_dir()
                && entry.path().join("module.yaml").exists()
                && let Some(name) = entry.file_name().to_str()
            {
                names.push(name.to_string());
            }
        }
        names.sort();
        Ok(names)
    }

    pub fn get_existing_profiles(&self) -> Result<Vec<String>, CfgdError> {
        let profiles_dir = self.repo_root.join("profiles");
        Ok(crate::config::scan_profiles(&profiles_dir)?
            .into_iter()
            .map(|e| e.name)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // 9.9.x sentinel per testing.md: fixture versions must never collide with
    // a real release stream.
    const VER: &str = "9.9.9";

    #[test]
    fn test_get_existing_modules_finds_modules() {
        let tmp = TempDir::new().unwrap();
        let nvim_dir = tmp.path().join("modules").join("nvim");
        std::fs::create_dir_all(&nvim_dir).unwrap();
        std::fs::write(nvim_dir.join("module.yaml"), "test").unwrap();
        let tmux_dir = tmp.path().join("modules").join("tmux");
        std::fs::create_dir_all(&tmux_dir).unwrap();
        std::fs::write(tmux_dir.join("module.yaml"), "test").unwrap();

        let session = GenerateSession::new(tmp.path().to_path_buf(), VER);
        let modules = session.get_existing_modules().unwrap();
        assert_eq!(modules, vec!["nvim", "tmux"]);
    }

    #[test]
    fn test_get_existing_profiles_finds_profiles() {
        let tmp = TempDir::new().unwrap();
        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(profiles_dir.join("base.yaml"), "test").unwrap();
        let canonical_dir = profiles_dir.join("work");
        std::fs::create_dir_all(&canonical_dir).unwrap();
        std::fs::write(canonical_dir.join("profile.yaml"), "test").unwrap();

        let session = GenerateSession::new(tmp.path().to_path_buf(), VER);
        let profiles = session.get_existing_profiles().unwrap();
        assert_eq!(profiles, vec!["base", "work"]);
    }

    #[test]
    fn test_write_module_yaml_valid() {
        let tmp = TempDir::new().unwrap();
        let mut session = GenerateSession::new(tmp.path().to_path_buf(), VER);
        let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: nvim\nspec:\n  packages:\n    - name: neovim\n";
        let path = session.write_module_yaml("nvim", yaml).unwrap();
        assert_eq!(path, tmp.path().join("modules/nvim/module.yaml"));
        assert!(path.exists());
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            written,
            format!(
                "{}{}",
                crate::config::schema_modeline(crate::config::SchemaDocKind::Module, VER),
                yaml
            )
        );
        assert_eq!(session.list_generated().len(), 1);
    }

    #[test]
    fn test_write_module_yaml_invalid_rejected() {
        let tmp = TempDir::new().unwrap();
        let mut session = GenerateSession::new(tmp.path().to_path_buf(), VER);
        let result = session.write_module_yaml("bad", "not valid yaml {{");
        assert!(result.is_err());
        assert!(session.list_generated().is_empty());
    }

    #[test]
    fn test_write_profile_yaml_valid() {
        let tmp = TempDir::new().unwrap();
        let mut session = GenerateSession::new(tmp.path().to_path_buf(), VER);
        let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: base\nspec:\n  modules:\n    - nvim\n";
        let path = session.write_profile_yaml("base", yaml).unwrap();
        assert_eq!(path, tmp.path().join("profiles/base/profile.yaml"));
        assert!(path.exists());
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            written.lines().next().unwrap(),
            crate::config::schema_modeline(crate::config::SchemaDocKind::Profile, VER).trim_end()
        );
        // Modeline is a YAML comment: the document must still parse.
        let parsed: serde_yaml::Value = serde_yaml::from_str(&written).unwrap();
        assert_eq!(parsed["kind"], serde_yaml::Value::from("Profile"));
        assert_eq!(session.list_generated().len(), 1);
    }

    #[test]
    fn test_write_profile_yaml_overwrites_legacy_form_without_canonical_twin() {
        let tmp = TempDir::new().unwrap();
        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        let legacy = profiles_dir.join("base.yaml");
        std::fs::write(
            &legacy,
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: base\nspec: {}\n",
        )
        .unwrap();

        let mut session = GenerateSession::new(tmp.path().to_path_buf(), VER);
        let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: base\nspec:\n  modules:\n    - nvim\n";
        let path = session.write_profile_yaml("base", yaml).unwrap();

        assert_eq!(path, legacy, "must write back to the found legacy form");
        let written = std::fs::read_to_string(&legacy).unwrap();
        assert!(
            written.starts_with("# yaml-language-server: $schema="),
            "generated write carries the schema modeline"
        );
        assert!(written.ends_with(yaml));
        assert!(
            !profiles_dir.join("base").exists(),
            "no canonical twin may be created beside a legacy manifest"
        );
    }

    #[test]
    fn test_write_profile_yaml_ambiguous_forms_propagate() {
        let tmp = TempDir::new().unwrap();
        let profiles_dir = tmp.path().join("profiles");
        let canonical_dir = profiles_dir.join("base");
        std::fs::create_dir_all(&canonical_dir).unwrap();
        std::fs::write(canonical_dir.join("profile.yaml"), "x").unwrap();
        std::fs::write(profiles_dir.join("base.yaml"), "x").unwrap();

        let mut session = GenerateSession::new(tmp.path().to_path_buf(), VER);
        let yaml =
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: base\nspec: {}\n";
        let err = session.write_profile_yaml("base", yaml).unwrap_err();
        assert!(
            err.to_string().contains("ambiguous"),
            "ambiguity must propagate, got: {err}"
        );
        assert!(session.list_generated().is_empty());
    }

    #[test]
    fn test_write_profile_yaml_wrong_kind_rejected() {
        let tmp = TempDir::new().unwrap();
        let mut session = GenerateSession::new(tmp.path().to_path_buf(), VER);
        let yaml =
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: nvim\nspec: {}\n";
        let result = session.write_profile_yaml("nvim", yaml);
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("Invalid profile YAML"),
            "expected validation error about wrong kind, got: {err_msg}"
        );
    }
}
