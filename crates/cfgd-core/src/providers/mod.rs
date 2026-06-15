// Provider traits and registry — consumed by packages/, files/, secrets/, reconciler/

pub mod skill;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use secrecy::SecretString;
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

    /// Map a profile package entry to the identity name that
    /// [`installed_packages`](Self::installed_packages) reports for it.
    ///
    /// Most managers install and list under the same name, so the default is
    /// identity. Managers whose install argument differs from the listed name —
    /// notably `go`, where `rsc.io/2fa@v1` installs but lists as the binary
    /// `2fa` — override this so install-diffing and prune compare like with
    /// like. The returned value is also the per-package tracking key suffix
    /// (`<manager>/<identity>`), keeping install-tracking and prune coherent.
    fn package_identity(&self, entry: &str) -> String {
        entry.to_string()
    }

    /// The uninstall command template to PERSIST alongside this package's tracking
    /// row, so the package can still be removed after its manager definition leaves
    /// the config. `None` for managers whose uninstall is derivable from code (every
    /// built-in manager); `Some` only for user-defined scripted managers, whose
    /// script vanishes with the config block.
    fn persisted_uninstall(&self) -> Option<String> {
        None
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

    /// Provide the active config directory so a configurator can resolve
    /// config-relative file paths (e.g. systemd `unitFile`) the same way file
    /// and secret sources are resolved. Most configurators reference no external
    /// files and keep the default no-op.
    fn set_config_dir(&mut self, _config_dir: &std::path::Path) {}
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

/// Content-drift outcome for a single managed file: whether the on-disk target
/// matches the rendered source content (presence AND bytes), plus the
/// human-readable `expected`/`actual` descriptions used to build a drift report.
///
/// `target` is the display path of the managed target. `matches` is `true` only
/// when the target exists and its bytes equal the rendered source; a missing
/// source or missing target yields `matches: false` with `actual` describing the
/// reason rather than an error.
#[derive(Debug, Clone)]
pub struct FileDriftResult {
    pub target: String,
    pub matches: bool,
    pub expected: String,
    pub actual: String,
}

pub trait FileManager: Send + Sync {
    fn scan_source(&self, layers: &[FileLayer]) -> Result<FileTree>;
    fn scan_target(&self, paths: &[PathBuf]) -> Result<FileTree>;
    fn diff(&self, source: &FileTree, target: &FileTree) -> Result<Vec<FileDiff>>;
    fn apply(&self, actions: &[FileAction], printer: &Printer) -> Result<()>;

    /// Content-aware drift check for a single source/target pair.
    ///
    /// Renders the source (tera template when applicable, otherwise read as-is)
    /// and byte-compares it to the on-disk target, returning a
    /// [`FileDriftResult`]. A missing source or missing target yields a
    /// non-matching result (`matches: false`) rather than an error, so a single
    /// unresolvable entry cannot mask drift elsewhere.
    fn content_drift(
        &self,
        source: &Path,
        target: &Path,
        origin: Option<&str>,
    ) -> Result<FileDriftResult>;
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

/// A tracked package whose custom/scripted manager has left the config. Carries
/// the manager and package names plus the uninstall command persisted at install
/// time (`None` for rows tracked before the persisted-uninstall column existed),
/// so the GC pass can run the script and prune the row — or warn when there is no
/// script to run. Crosses the `DaemonHooks` boundary, so it lives here alongside
/// [`PackageAction`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanedPackage {
    pub manager: String,
    pub package: String,
    pub uninstall_cmd: Option<String>,
}

// --- SecretBackend trait ---

pub trait SecretBackend: Send + Sync {
    fn name(&self) -> &str;
    fn is_available(&self) -> bool;
    fn encrypt_file(&self, path: &Path) -> Result<()>;
    fn decrypt_file(&self, path: &Path) -> Result<SecretString>;
    fn edit_file(&self, path: &Path) -> Result<()>;
}

// --- SecretProvider trait ---

pub trait SecretProvider: Send + Sync {
    fn name(&self) -> &str;
    fn is_available(&self) -> bool;
    fn resolve(&self, reference: &str) -> Result<SecretString>;
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
    /// Resolve a secret and inject its value as environment variables into the
    /// managed shell env file (`~/.cfgd.env`, `~/.cfgd-env.ps1`, fish conf.d).
    ResolveEnv {
        provider: String,
        reference: String,
        envs: Vec<String>,
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

    /// The set of registered (config-present) package-manager names — used to
    /// detect orphaned tracked packages whose custom manager left the config.
    pub fn manager_names(&self) -> HashSet<String> {
        self.package_managers
            .iter()
            .map(|m| m.name().to_string())
            .collect()
    }

    /// Hand the active config directory to every system configurator so those
    /// that reference config-relative files (e.g. systemd `unitFile`) resolve
    /// them against the config dir rather than the process CWD.
    pub fn set_system_config_dir(&mut self, config_dir: &std::path::Path) {
        for sc in self.system_configurators.iter_mut() {
            sc.set_config_dir(config_dir);
        }
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

/// Configurable mock for `PackageManager`. Available to all test modules within cfgd-core.
#[cfg(test)]
pub(crate) struct StubPackageManager {
    pub name: String,
    pub available: bool,
    pub installed: HashSet<String>,
    pub versions: std::collections::HashMap<String, String>,
    pub bootstrap_capable: bool,
    /// When Some, `installed_packages()` returns an Err carrying this message.
    /// Lets tests drive the "cannot query" arms in compliance + reconciler code.
    pub installed_error: Option<String>,
}

#[cfg(test)]
impl StubPackageManager {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            available: true,
            installed: HashSet::new(),
            versions: std::collections::HashMap::new(),
            bootstrap_capable: false,
            installed_error: None,
        }
    }

    pub fn unavailable(mut self) -> Self {
        self.available = false;
        self
    }

    pub fn bootstrappable(mut self) -> Self {
        self.bootstrap_capable = true;
        self
    }

    pub fn with_installed(mut self, pkgs: &[&str]) -> Self {
        for p in pkgs {
            self.installed.insert((*p).to_string());
        }
        self
    }

    pub fn with_installed_error(mut self, message: &str) -> Self {
        self.installed_error = Some(message.to_string());
        self
    }

    pub fn with_package(mut self, pkg: &str, ver: &str) -> Self {
        self.versions.insert(pkg.to_string(), ver.to_string());
        self
    }
}

#[cfg(test)]
impl PackageManager for StubPackageManager {
    fn name(&self) -> &str {
        &self.name
    }
    fn is_available(&self) -> bool {
        self.available
    }
    fn can_bootstrap(&self) -> bool {
        self.bootstrap_capable
    }
    fn bootstrap(&self, _printer: &Printer) -> Result<()> {
        Ok(())
    }
    fn installed_packages(&self) -> Result<HashSet<String>> {
        if let Some(ref msg) = self.installed_error {
            return Err(crate::errors::CfgdError::Io(std::io::Error::other(
                msg.clone(),
            )));
        }
        Ok(self.installed.clone())
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
    fn available_version(&self, package: &str) -> Result<Option<String>> {
        Ok(self.versions.get(package).cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_filters_available_managers() {
        let mut registry = ProviderRegistry::new();
        registry
            .package_managers
            .push(Box::new(StubPackageManager::new("mock")));
        registry
            .package_managers
            .push(Box::new(StubPackageManager::new("mock2").unavailable()));

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
        let mock = StubPackageManager::new("mock");
        let pkgs = mock.installed_packages_with_versions().unwrap();
        assert!(pkgs.is_empty());
    }

    #[test]
    fn test_default_package_aliases_empty() {
        let mock = StubPackageManager::new("mock");
        let aliases = mock.package_aliases("fd").unwrap();
        assert!(aliases.is_empty());
    }

    #[test]
    fn parse_secret_reference_1password() {
        let (provider, rest) = parse_secret_reference("1password://Vault/Item/Field").unwrap();
        assert_eq!(provider, "1password");
        assert_eq!(rest, "Vault/Item/Field");
    }

    #[test]
    fn parse_secret_reference_bitwarden() {
        let (provider, rest) = parse_secret_reference("bitwarden://folder/item").unwrap();
        assert_eq!(provider, "bitwarden");
        assert_eq!(rest, "folder/item");
    }

    #[test]
    fn parse_secret_reference_lastpass() {
        let (provider, rest) = parse_secret_reference("lastpass://folder/item/field").unwrap();
        assert_eq!(provider, "lastpass");
        assert_eq!(rest, "folder/item/field");
    }

    #[test]
    fn parse_secret_reference_vault() {
        let (provider, rest) = parse_secret_reference("vault://secret/path#field").unwrap();
        assert_eq!(provider, "vault");
        assert_eq!(rest, "secret/path#field");
    }

    #[test]
    fn parse_secret_reference_unknown_returns_none() {
        assert!(parse_secret_reference("plaintext").is_none());
        assert!(parse_secret_reference("file:///etc/passwd").is_none());
        assert!(parse_secret_reference("").is_none());
    }

    #[test]
    fn provider_registry_default_matches_new() {
        let reg = ProviderRegistry::default();
        assert!(reg.package_managers.is_empty());
        assert!(reg.system_configurators.is_empty());
        assert!(reg.file_manager.is_none());
        assert!(reg.secret_backend.is_none());
        assert!(reg.secret_providers.is_empty());
    }

    struct StubConfigurator {
        name: String,
        available: bool,
    }

    impl SystemConfigurator for StubConfigurator {
        fn name(&self) -> &str {
            &self.name
        }
        fn is_available(&self) -> bool {
            self.available
        }
        fn current_state(&self) -> Result<serde_yaml::Value> {
            Ok(serde_yaml::Value::Null)
        }
        fn diff(&self, _desired: &serde_yaml::Value) -> Result<Vec<SystemDrift>> {
            Ok(Vec::new())
        }
        fn apply(&self, _desired: &serde_yaml::Value, _printer: &Printer) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn available_system_configurators_filters_unavailable() {
        let mut reg = ProviderRegistry::new();
        reg.system_configurators.push(Box::new(StubConfigurator {
            name: "shell".to_string(),
            available: true,
        }));
        reg.system_configurators.push(Box::new(StubConfigurator {
            name: "systemd".to_string(),
            available: false,
        }));

        let available = reg.available_system_configurators();
        assert_eq!(available.len(), 1);
        assert_eq!(available[0].name(), "shell");
    }

    #[test]
    fn stub_builder_chain_full() {
        let stub = StubPackageManager::new("brew")
            .bootstrappable()
            .with_installed(&["jq", "ripgrep"])
            .with_package("jq", "1.7.1");
        assert!(stub.is_available());
        assert!(stub.can_bootstrap());
        assert_eq!(stub.installed_packages().unwrap().len(), 2);
        assert_eq!(
            stub.available_version("jq").unwrap(),
            Some("1.7.1".to_string())
        );
        assert!(stub.available_version("missing").unwrap().is_none());
    }

    #[test]
    fn stub_with_installed_error_returns_err() {
        let stub =
            StubPackageManager::new("brew").with_installed_error("simulated brew list failure");
        let err = stub.installed_packages().unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("simulated brew list failure"),
            "unexpected error message: {msg}"
        );
    }

    #[test]
    fn stub_default_installed_packages_with_versions_with_content() {
        let stub = StubPackageManager::new("brew").with_installed(&["fd", "jq"]);
        let mut pkgs = stub.installed_packages_with_versions().unwrap();
        pkgs.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(pkgs.len(), 2);
        assert_eq!(pkgs[0].name, "fd");
        assert_eq!(pkgs[0].version, "unknown");
        assert_eq!(pkgs[1].name, "jq");
        assert_eq!(pkgs[1].version, "unknown");
    }

    #[test]
    fn stub_default_path_dirs_empty() {
        let stub = StubPackageManager::new("apt");
        assert!(stub.path_dirs().is_empty());
    }

    #[test]
    fn package_info_serde_round_trips() {
        let info = PackageInfo {
            name: "jq".to_string(),
            version: "1.7.1".to_string(),
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("\"name\":\"jq\""));
        assert!(json.contains("\"version\":\"1.7.1\""));
        let parsed: PackageInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, info.name);
        assert_eq!(parsed.version, info.version);
    }
}
