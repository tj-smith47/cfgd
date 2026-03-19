// Provider traits and registry — consumed by packages/, files/, secrets/, reconciler/

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::errors::Result;
use crate::output::Printer;

// --- PackageManager trait ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageInfo {
    pub name: String,
    pub version: String,
}

pub trait PackageManager: Send + Sync {
    fn name(&self) -> &str;
    fn is_available(&self) -> bool;
    fn can_bootstrap(&self) -> bool;
    fn bootstrap(&self, printer: &Printer) -> Result<()>;
    fn installed_packages(&self) -> Result<HashSet<String>>;
    fn install(&self, packages: &[String], printer: &Printer) -> Result<()>;
    fn uninstall(&self, packages: &[String], printer: &Printer) -> Result<()>;
    fn update(&self, printer: &Printer) -> Result<()>;

    /// Query the available version of a package without installing it.
    /// Returns None if the package is not found in the manager's index.
    fn available_version(&self, package: &str) -> Result<Option<String>>;

    /// Directories to add to PATH after bootstrap. Empty for managers
    /// that are already on the system PATH (apt, dnf, etc.).
    fn path_dirs(&self) -> Vec<String> {
        Vec::new()
    }

    /// List all installed packages with their installed versions.
    /// Default implementation wraps `installed_packages()` with version "unknown".
    fn installed_packages_with_versions(&self) -> Result<Vec<PackageInfo>> {
        Ok(self
            .installed_packages()?
            .into_iter()
            .map(|name| PackageInfo {
                name,
                version: "unknown".into(),
            })
            .collect())
    }

    /// Return alternative names / aliases for a canonical package name.
    /// Used for cross-manager package resolution. Default returns empty.
    fn package_aliases(&self, _canonical_name: &str) -> Result<Vec<String>> {
        Ok(vec![])
    }
}

// --- SystemConfigurator trait ---

pub struct SystemDrift {
    pub key: String,
    pub expected: String,
    pub actual: String,
}

pub trait SystemConfigurator: Send + Sync {
    /// Configurator name — must match the key in the profile's `system:` map
    fn name(&self) -> &str;
    fn is_available(&self) -> bool;

    /// Read current state from the system
    fn current_state(&self) -> Result<serde_yaml::Value>;

    /// Diff desired vs actual, return list of changes
    fn diff(&self, desired: &serde_yaml::Value) -> Result<Vec<SystemDrift>>;

    /// Apply desired state
    fn apply(&self, desired: &serde_yaml::Value, printer: &Printer) -> Result<()>;
}

// --- FileManager trait ---

use std::collections::BTreeMap;

#[derive(Debug)]
pub struct FileLayer {
    pub source_dir: PathBuf,
    pub origin_source: String,
    pub priority: u32,
}

#[derive(Debug)]
pub struct FileTree {
    pub files: BTreeMap<PathBuf, FileEntry>,
}

#[derive(Debug)]
pub struct FileEntry {
    pub content_hash: String,
    pub permissions: Option<u32>,
    pub is_template: bool,
    pub source_path: PathBuf,
    pub origin_source: String,
}

#[derive(Debug)]
pub struct FileDiff {
    pub target: PathBuf,
    pub kind: FileDiffKind,
}

#[derive(Debug)]
pub enum FileDiffKind {
    Created { source: PathBuf },
    Modified { source: PathBuf, diff: String },
    Deleted,
    PermissionsChanged { current: u32, desired: u32 },
    Unchanged,
}

#[derive(Debug, Serialize)]
pub enum FileAction {
    Create {
        source: PathBuf,
        target: PathBuf,
        origin: String,
        strategy: crate::config::FileStrategy,
        /// SHA256 of source content at plan time (for TOCTOU verification).
        source_hash: Option<String>,
    },
    Update {
        source: PathBuf,
        target: PathBuf,
        diff: String,
        origin: String,
        strategy: crate::config::FileStrategy,
        /// SHA256 of source content at plan time (for TOCTOU verification).
        source_hash: Option<String>,
    },
    Delete {
        target: PathBuf,
        origin: String,
    },
    SetPermissions {
        target: PathBuf,
        mode: u32,
        origin: String,
    },
    Skip {
        target: PathBuf,
        reason: String,
        origin: String,
    },
}

pub trait FileManager: Send + Sync {
    fn scan_source(&self, layers: &[FileLayer]) -> Result<FileTree>;
    fn scan_target(&self, paths: &[PathBuf]) -> Result<FileTree>;
    fn diff(&self, source: &FileTree, target: &FileTree) -> Result<Vec<FileDiff>>;
    fn apply(&self, actions: &[FileAction], printer: &Printer) -> Result<()>;
}

// --- PackageAction ---

#[derive(Debug, Serialize)]
pub enum PackageAction {
    Bootstrap {
        manager: String,
        method: String,
        origin: String,
    },
    Install {
        manager: String,
        packages: Vec<String>,
        origin: String,
    },
    Uninstall {
        manager: String,
        packages: Vec<String>,
        origin: String,
    },
    Skip {
        manager: String,
        reason: String,
        origin: String,
    },
}

// --- SecretBackend trait ---

pub trait SecretBackend: Send + Sync {
    fn name(&self) -> &str;
    fn is_available(&self) -> bool;
    fn encrypt_file(&self, path: &Path) -> Result<()>;
    fn decrypt_file(&self, path: &Path) -> Result<String>;
    fn edit_file(&self, path: &Path) -> Result<()>;
}

// --- SecretProvider trait ---

pub trait SecretProvider: Send + Sync {
    fn name(&self) -> &str;
    fn is_available(&self) -> bool;
    fn resolve(&self, reference: &str) -> Result<String>;
}

// --- SecretAction ---

#[derive(Debug, Serialize)]
pub enum SecretAction {
    Decrypt {
        source: PathBuf,
        target: PathBuf,
        backend: String,
        origin: String,
    },
    Resolve {
        provider: String,
        reference: String,
        target: PathBuf,
        origin: String,
    },
    Skip {
        source: String,
        reason: String,
        origin: String,
    },
}

// --- ProviderRegistry ---

pub struct ProviderRegistry {
    pub package_managers: Vec<Box<dyn PackageManager>>,
    pub system_configurators: Vec<Box<dyn SystemConfigurator>>,
    pub file_manager: Option<Box<dyn FileManager>>,
    pub secret_backend: Option<Box<dyn SecretBackend>>,
    pub secret_providers: Vec<Box<dyn SecretProvider>>,
    pub default_file_strategy: crate::config::FileStrategy,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            package_managers: Vec::new(),
            system_configurators: Vec::new(),
            file_manager: None,
            secret_backend: None,
            secret_providers: Vec::new(),
            default_file_strategy: crate::config::FileStrategy::Symlink,
        }
    }

    pub fn available_package_managers(&self) -> Vec<&dyn PackageManager> {
        self.package_managers
            .iter()
            .filter(|pm| pm.is_available())
            .map(|pm| pm.as_ref())
            .collect()
    }

    pub fn available_system_configurators(&self) -> Vec<&dyn SystemConfigurator> {
        self.system_configurators
            .iter()
            .filter(|sc| sc.is_available())
            .map(|sc| sc.as_ref())
            .collect()
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse a secret reference string to determine the provider.
/// Formats:
///   - `1password://Vault/Item/Field` → 1Password
///   - `bitwarden://folder/item` → Bitwarden
///   - `lastpass://folder/item/field` → LastPass
///   - `vault://secret/path#field` → HashiCorp Vault
///
/// Returns (provider_name, reference_path).
pub fn parse_secret_reference(source: &str) -> Option<(&str, &str)> {
    if let Some(rest) = source.strip_prefix("1password://") {
        Some(("1password", rest))
    } else if let Some(rest) = source.strip_prefix("bitwarden://") {
        Some(("bitwarden", rest))
    } else if let Some(rest) = source.strip_prefix("lastpass://") {
        Some(("lastpass", rest))
    } else if let Some(rest) = source.strip_prefix("vault://") {
        Some(("vault", rest))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockPackageManager {
        available: bool,
    }

    impl PackageManager for MockPackageManager {
        fn name(&self) -> &str {
            "mock"
        }
        fn is_available(&self) -> bool {
            self.available
        }
        fn can_bootstrap(&self) -> bool {
            false
        }
        fn bootstrap(&self, _printer: &Printer) -> Result<()> {
            Ok(())
        }
        fn installed_packages(&self) -> Result<HashSet<String>> {
            Ok(HashSet::new())
        }
        fn install(&self, _packages: &[String], _printer: &Printer) -> Result<()> {
            Ok(())
        }
        fn uninstall(&self, _packages: &[String], _printer: &Printer) -> Result<()> {
            Ok(())
        }
        fn update(&self, _printer: &Printer) -> Result<()> {
            Ok(())
        }
        fn available_version(&self, _package: &str) -> Result<Option<String>> {
            Ok(None)
        }
    }

    #[test]
    fn registry_filters_available_managers() {
        let mut registry = ProviderRegistry::new();
        registry
            .package_managers
            .push(Box::new(MockPackageManager { available: true }));
        registry
            .package_managers
            .push(Box::new(MockPackageManager { available: false }));

        let available = registry.available_package_managers();
        assert_eq!(available.len(), 1);
        assert_eq!(available[0].name(), "mock");
    }

    #[test]
    fn empty_registry() {
        let registry = ProviderRegistry::new();
        assert!(registry.available_package_managers().is_empty());
        assert!(registry.available_system_configurators().is_empty());
        assert!(registry.file_manager.is_none());
        assert!(registry.secret_backend.is_none());
    }

    #[test]
    fn test_default_installed_packages_with_versions_empty() {
        let mock = MockPackageManager { available: true };
        let pkgs = mock.installed_packages_with_versions().unwrap();
        assert!(pkgs.is_empty());
    }

    #[test]
    fn test_default_package_aliases_empty() {
        let mock = MockPackageManager { available: true };
        let aliases = mock.package_aliases("fd").unwrap();
        assert!(aliases.is_empty());
    }
}
