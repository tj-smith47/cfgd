// Provider traits and registry — consumed by packages/, files/, secrets/, reconciler/

pub mod skill;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use secrecy::SecretString;
use serde::{Deserialize, Serialize};

use crate::errors::Result;
use crate::output::{Printer, Role};

// --- PackageManager trait ---

/// The slice of persisted state a `PackageManager` may reach.
///
/// Declared here rather than importing `state::StateStore` so the trait layer
/// names the capability it needs and the storage layer supplies it — the
/// dependency runs `state` → `providers`, not the other way round. `StateStore`
/// implements this alongside its inherent methods.
pub trait PackageStateStore {
    /// The prefix previously resolved for `manager`, paired with whether it was
    /// the fallback rather than the manager's own configured prefix. `None` when
    /// nothing has been resolved yet.
    fn resolved_prefix(&self, manager: &str) -> Result<Option<(String, bool)>>;

    /// Record the global-install prefix `manager` resolved, replacing any
    /// earlier record for it.
    fn record_resolved_prefix(&self, manager: &str, prefix: &str, is_fallback: bool) -> Result<()>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageInfo {
    pub name: String,
    pub version: String,
}

/// The per-run context every state-touching `PackageManager` method receives:
/// the printer for user-facing output, plus the reconciler's already-open
/// `StateStore` connection. Threading `state` here — instead of a manager
/// opening its own `StateStore::open_default()` — is what makes a
/// `--state-dir`-scoped run's package-manager state (e.g. npm's persisted
/// global-prefix decision) honor that override rather than silently reading
/// and writing the default state location out from under it, and avoids a
/// second SQLite connection contending with the reconciler's own
/// `BEGIN EXCLUSIVE` writes. Mirrors the "no process-global mutable state"
/// precedent set by `register_bootstrapped_path_dirs`/`PATH_ENV_LOCK` — this
/// is struct-threaded, not global.
pub struct PackageContext<'a> {
    pub printer: &'a Printer,
    pub state: &'a dyn PackageStateStore,
    pub notes: &'a NoteSink,
    /// True when something further up already emits one status line for the
    /// action this call is part of, so a command's live-output window must
    /// collapse silently ([`Printer::run_silent`]) instead of settling a second
    /// line for the same work. Set by [`PackageContext::caller_owns_status`];
    /// false everywhere else, where a manager's command IS the action.
    pub caller_owns_status: bool,
}

impl<'a> PackageContext<'a> {
    /// A context that collects no post-install notes — every read path, and
    /// every fixture. Notes pushed through it are discarded rather than
    /// retained, because nothing will drain them.
    pub fn new(printer: &'a Printer, state: &'a dyn PackageStateStore) -> Self {
        Self {
            printer,
            state,
            notes: NoteSink::discarded(),
            caller_owns_status: false,
        }
    }

    /// A context whose notes travel back to the caller — the reconciler's
    /// install paths, where the drained notes render under the action's status.
    pub fn with_notes(
        printer: &'a Printer,
        state: &'a dyn PackageStateStore,
        notes: &'a NoteSink,
    ) -> Self {
        Self {
            printer,
            state,
            notes,
            caller_owns_status: false,
        }
    }

    /// Declare that the CALLER emits the one status line for this action.
    ///
    /// The reconciler's tree renders `✓ brew install ripgrep` from the plan,
    /// with the phase's alignment column and any drained notes under it. A
    /// manager command run under such a context shows its live window and then
    /// collapses without a line, so the action renders once rather than twice.
    #[must_use]
    pub fn caller_owns_status(mut self) -> Self {
        self.caller_owns_status = true;
        self
    }

    /// Report something that is NOT this action's status line — work done on
    /// the side, a degraded fallback, the expanded command a custom manager
    /// ran.
    ///
    /// The counterpart of `pkg_run` for prose: under a caller-owned status the
    /// message becomes a note rendered UNDER the action's one line, so a
    /// manager cannot add a second settled line beside the tree's; standalone
    /// it settles on its own, where the manager's output is all the user gets.
    /// A `PackageManager` must never call `cx.printer.status_simple` directly.
    pub fn report(&self, role: Role, manager: &str, message: impl Into<String>) {
        let message = message.into();
        if self.caller_owns_status {
            self.notes.push(PostInstallNote {
                manager: manager.to_string(),
                message,
                role,
            });
        } else {
            self.printer.status_simple(role, message);
        }
    }
}

/// An important message a package manager emitted while one action ran — a brew
/// caveat, an npm warning. Part of the provider contract rather than of any one
/// manager, because the reconciler is what renders it: the note is attached to
/// the action that produced it instead of being printed from inside the manager,
/// where it would land above the action's own status line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostInstallNote {
    pub manager: String,
    pub message: String,
    /// How the note renders under the action's line. A caveat or a degraded
    /// fallback is a [`Role::Warn`]; a report of work the manager did on the
    /// side is a [`Role::Info`].
    pub role: Role,
}

impl PostInstallNote {
    /// A note the user must act on — a caveat, a fallback, a retry.
    pub fn warn(manager: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            manager: manager.into(),
            message: message.into(),
            role: Role::Warn,
        }
    }

    /// A note that only reports what happened.
    pub fn info(manager: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            manager: manager.into(),
            message: message.into(),
            role: Role::Info,
        }
    }
}

/// Collector for the notes a manager produced during one action.
///
/// Interior mutability because every `PackageManager` method takes
/// `&PackageContext`.
pub struct NoteSink {
    notes: std::sync::Mutex<Vec<PostInstallNote>>,
    /// A sink nobody drains discards instead of growing. [`NoteSink::discarded`]
    /// hands out one `&'static` sink to every non-collecting context, so
    /// retaining pushes in it would be an unbounded leak with no reader — and
    /// storing nothing is also what keeps it from being the process-global
    /// mutable state [`PackageContext`]'s own contract rules out.
    collecting: bool,
}

impl Default for NoteSink {
    fn default() -> Self {
        Self {
            notes: std::sync::Mutex::new(Vec::new()),
            collecting: true,
        }
    }
}

impl NoteSink {
    /// The shared sink for a context that collects nothing. Every push into it
    /// is dropped, so it holds no state and needs no drain.
    pub fn discarded() -> &'static NoteSink {
        static DISCARDED: std::sync::OnceLock<NoteSink> = std::sync::OnceLock::new();
        DISCARDED.get_or_init(|| NoteSink {
            notes: std::sync::Mutex::new(Vec::new()),
            collecting: false,
        })
    }

    pub fn push(&self, note: PostInstallNote) {
        if !self.collecting {
            return;
        }
        self.notes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(note);
    }

    /// Drain — called by the reconciler once per action, after its status.
    pub fn take(&self) -> Vec<PostInstallNote> {
        std::mem::take(&mut *self.notes.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

pub trait PackageManager: Send + Sync {
    fn name(&self) -> &str;
    fn is_available(&self) -> bool;
    fn can_bootstrap(&self) -> bool;
    fn bootstrap(&self, cx: &PackageContext<'_>) -> Result<()>;
    fn installed_packages(&self, cx: &PackageContext<'_>) -> Result<HashSet<String>>;
    fn install(&self, packages: &[String], cx: &PackageContext<'_>) -> Result<()>;
    fn uninstall(&self, packages: &[String], cx: &PackageContext<'_>) -> Result<()>;
    fn update(&self, cx: &PackageContext<'_>) -> Result<()>;

    /// Query the available version of a package without installing it.
    /// Returns None if the package is not found in the manager's index.
    fn available_version(&self, package: &str) -> Result<Option<String>>;

    /// Whether `available` satisfies a `>= min_version` floor. The default
    /// compares as loose semver. Managers whose version scheme is not semver —
    /// notably FreeBSD `pkg`, whose versions carry PORTEPOCH (`,N`) and
    /// PORTREVISION (`_N`) suffixes — override this to defer to the manager's
    /// own version comparator so the floor is evaluated correctly.
    fn version_meets_minimum(&self, available: &str, min_version: &str) -> bool {
        crate::version_satisfies(available, &format!(">={min_version}"))
    }

    /// Directories to add to PATH after bootstrap. Empty for managers
    /// that are already on the system PATH (apt, dnf, etc.).
    fn path_dirs(&self, cx: &PackageContext<'_>) -> Vec<String> {
        let _ = cx;
        Vec::new()
    }

    /// List all installed packages with their installed versions.
    /// Default implementation wraps `installed_packages()` with version "unknown".
    fn installed_packages_with_versions(
        &self,
        cx: &PackageContext<'_>,
    ) -> Result<Vec<PackageInfo>> {
        Ok(self
            .installed_packages(cx)?
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

    /// Apply desired state.
    ///
    /// The CALLER owns the action's status line: the reconciler settles one
    /// `system:<name>.<key>` line for this call from the plan. A shell-out here
    /// therefore goes through [`Printer::run_silent`] — [`Printer::run`] settles
    /// the window's own line too, rendering the same work twice.
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
        /// Merge spec carried from the profile entry, set exactly when
        /// `strategy` is `Patch`. Apply re-runs it against the target's live
        /// content, so `source` is empty and `source_hash` is `None`.
        #[serde(skip_serializing_if = "Option::is_none")]
        patch: Option<crate::config::PatchSpec>,
    },
    Update {
        source: PathBuf,
        target: PathBuf,
        diff: String,
        origin: String,
        strategy: crate::config::FileStrategy,
        /// SHA256 of source content at plan time (for TOCTOU verification).
        source_hash: Option<String>,
        /// See [`FileAction::Create::patch`].
        #[serde(skip_serializing_if = "Option::is_none")]
        patch: Option<crate::config::PatchSpec>,
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

/// Serialized in the same shape as a `VerifyResult`: `resourceType` is the
/// constant `"file"` and the target path is the `resourceId`.
///
/// Hand-written rather than derived because `resourceType` is not a field —
/// making it one would let a construction site set it to something other than
/// `"file"`, and the whole point is that a structured consumer can read a
/// drifted file and a failed verify check through one code path.
impl serde::Serialize for FileDriftResult {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("FileDriftResult", 5)?;
        state.serialize_field("resourceType", "file")?;
        state.serialize_field("resourceId", &self.target)?;
        state.serialize_field("matches", &self.matches)?;
        state.serialize_field("expected", &self.expected)?;
        state.serialize_field("actual", &self.actual)?;
        state.end()
    }
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
    /// When true, `package_identity` lowercases its entry, mimicking a
    /// case-insensitive manager (choco/scoop/winget) whose `installed_packages`
    /// reports a different letter case than the desired name. Lets tests guard
    /// the identity-routed comparison sites (verify, compliance, diff).
    pub fold_case: bool,
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
            fold_case: false,
        }
    }

    /// Mark this stub as a case-insensitive manager so `package_identity`
    /// lowercases desired names before comparison.
    pub fn case_folding(mut self) -> Self {
        self.fold_case = true;
        self
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
    fn bootstrap(&self, _cx: &PackageContext<'_>) -> Result<()> {
        Ok(())
    }
    fn installed_packages(&self, _cx: &PackageContext<'_>) -> Result<HashSet<String>> {
        if let Some(ref msg) = self.installed_error {
            return Err(crate::errors::CfgdError::Io(std::io::Error::other(
                msg.clone(),
            )));
        }
        Ok(self.installed.clone())
    }
    fn install(&self, _packages: &[String], _cx: &PackageContext<'_>) -> Result<()> {
        Ok(())
    }
    fn uninstall(&self, _packages: &[String], _cx: &PackageContext<'_>) -> Result<()> {
        Ok(())
    }
    fn update(&self, _cx: &PackageContext<'_>) -> Result<()> {
        Ok(())
    }
    fn available_version(&self, package: &str) -> Result<Option<String>> {
        Ok(self.versions.get(package).cloned())
    }
    fn package_identity(&self, entry: &str) -> String {
        if self.fold_case {
            entry.to_ascii_lowercase()
        } else {
            entry.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // The trait layer must not depend on the store, but its own tests need a
    // real implementation to hand `PackageContext`; the import is test-only.
    use crate::state::StateStore;

    fn test_cx<'a>(printer: &'a Printer, state: &'a StateStore) -> PackageContext<'a> {
        PackageContext::new(printer, state)
    }

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
        let printer = crate::output::Printer::new(crate::output::Verbosity::Quiet);
        let state = StateStore::open_in_memory().unwrap();
        let pkgs = mock
            .installed_packages_with_versions(&test_cx(&printer, &state))
            .unwrap();
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
        let printer = crate::output::Printer::new(crate::output::Verbosity::Quiet);
        let state = StateStore::open_in_memory().unwrap();
        assert!(stub.is_available());
        assert!(stub.can_bootstrap());
        assert_eq!(
            stub.installed_packages(&test_cx(&printer, &state))
                .unwrap()
                .len(),
            2
        );
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
        let printer = crate::output::Printer::new(crate::output::Verbosity::Quiet);
        let state = StateStore::open_in_memory().unwrap();
        let err = stub
            .installed_packages(&test_cx(&printer, &state))
            .unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("simulated brew list failure"),
            "unexpected error message: {msg}"
        );
    }

    #[test]
    fn stub_default_installed_packages_with_versions_with_content() {
        let stub = StubPackageManager::new("brew").with_installed(&["fd", "jq"]);
        let printer = crate::output::Printer::new(crate::output::Verbosity::Quiet);
        let state = StateStore::open_in_memory().unwrap();
        let mut pkgs = stub
            .installed_packages_with_versions(&test_cx(&printer, &state))
            .unwrap();
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
        let printer = crate::output::Printer::new(crate::output::Verbosity::Quiet);
        let state = StateStore::open_in_memory().unwrap();
        assert!(stub.path_dirs(&test_cx(&printer, &state)).is_empty());
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
