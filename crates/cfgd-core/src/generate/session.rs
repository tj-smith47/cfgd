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
    generated: HashMap<String, GeneratedItem>,
}

#[derive(Debug, Clone)]
pub struct GeneratedItem {
    pub kind: SchemaKind,
    pub name: String,
    pub path: PathBuf,
}

impl GenerateSession {
    pub fn new(repo_root: PathBuf) -> Self {
        Self {
            repo_root,
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
        atomic_write_str(&path, content)?;
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
        let dir = self.repo_root.join("profiles");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.yaml", name));
        atomic_write_str(&path, content)?;
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
        if !profiles_dir.exists() {
            return Ok(vec![]);
        }
        let mut names = vec![];
        for entry in std::fs::read_dir(&profiles_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("yaml")
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                names.push(stem.to_string());
            }
        }
        names.sort();
        Ok(names)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_session_new() {
        let tmp = TempDir::new().unwrap();
        let session = GenerateSession::new(tmp.path().to_path_buf());
        assert_eq!(session.repo_root(), tmp.path());
        assert!(session.list_generated().is_empty());
    }

    #[test]
    fn test_get_existing_modules_empty() {
        let tmp = TempDir::new().unwrap();
        let session = GenerateSession::new(tmp.path().to_path_buf());
        let modules = session.get_existing_modules().unwrap();
        assert!(modules.is_empty());
    }

    #[test]
    fn test_get_existing_profiles_empty() {
        let tmp = TempDir::new().unwrap();
        let session = GenerateSession::new(tmp.path().to_path_buf());
        let profiles = session.get_existing_profiles().unwrap();
        assert!(profiles.is_empty());
    }

    #[test]
    fn test_get_existing_modules_finds_modules() {
        let tmp = TempDir::new().unwrap();
        let nvim_dir = tmp.path().join("modules").join("nvim");
        std::fs::create_dir_all(&nvim_dir).unwrap();
        std::fs::write(nvim_dir.join("module.yaml"), "test").unwrap();
        let tmux_dir = tmp.path().join("modules").join("tmux");
        std::fs::create_dir_all(&tmux_dir).unwrap();
        std::fs::write(tmux_dir.join("module.yaml"), "test").unwrap();

        let session = GenerateSession::new(tmp.path().to_path_buf());
        let modules = session.get_existing_modules().unwrap();
        assert_eq!(modules, vec!["nvim", "tmux"]);
    }

    #[test]
    fn test_get_existing_profiles_finds_profiles() {
        let tmp = TempDir::new().unwrap();
        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(profiles_dir.join("base.yaml"), "test").unwrap();
        std::fs::write(profiles_dir.join("work.yaml"), "test").unwrap();

        let session = GenerateSession::new(tmp.path().to_path_buf());
        let profiles = session.get_existing_profiles().unwrap();
        assert_eq!(profiles, vec!["base", "work"]);
    }

    #[test]
    fn test_write_module_yaml_valid() {
        let tmp = TempDir::new().unwrap();
        let mut session = GenerateSession::new(tmp.path().to_path_buf());
        let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: nvim\nspec:\n  packages:\n    - name: neovim\n";
        let path = session.write_module_yaml("nvim", yaml).unwrap();
        assert_eq!(path, tmp.path().join("modules/nvim/module.yaml"));
        assert!(path.exists());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), yaml);
        assert_eq!(session.list_generated().len(), 1);
    }

    #[test]
    fn test_write_module_yaml_invalid_rejected() {
        let tmp = TempDir::new().unwrap();
        let mut session = GenerateSession::new(tmp.path().to_path_buf());
        let result = session.write_module_yaml("bad", "not valid yaml {{");
        assert!(result.is_err());
        assert!(session.list_generated().is_empty());
    }

    #[test]
    fn test_write_profile_yaml_valid() {
        let tmp = TempDir::new().unwrap();
        let mut session = GenerateSession::new(tmp.path().to_path_buf());
        let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: base\nspec:\n  modules:\n    - nvim\n";
        let path = session.write_profile_yaml("base", yaml).unwrap();
        assert_eq!(path, tmp.path().join("profiles/base.yaml"));
        assert!(path.exists());
        assert_eq!(session.list_generated().len(), 1);
    }

    #[test]
    fn test_write_profile_yaml_wrong_kind_rejected() {
        let tmp = TempDir::new().unwrap();
        let mut session = GenerateSession::new(tmp.path().to_path_buf());
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
