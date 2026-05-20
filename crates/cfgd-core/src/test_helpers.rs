// Reusable test mocks and builders — gated behind `test-helpers` feature.
//
// Provides mock implementations of the core provider traits (FileManager,
// SecretBackend, SecretProvider, SystemConfigurator) plus a TestEnvBuilder
// for creating isolated temp directories with config/profile/module layouts.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use secrecy::SecretString;

use crate::errors::{CfgdError, FileError, SecretError};
use crate::output::Printer;
use crate::providers::{
    FileAction, FileDiff, FileLayer, FileTree, SecretBackend, SecretProvider, SystemConfigurator,
    SystemDrift,
};

// ---------------------------------------------------------------------------
// MockFileManager
// ---------------------------------------------------------------------------

/// Records calls to `FileManager` methods and returns configurable results.
pub struct MockFileManager {
    pub scan_source_calls: Mutex<Vec<String>>,
    pub scan_target_calls: Mutex<Vec<String>>,
    pub diff_calls: Mutex<Vec<String>>,
    pub apply_calls: Mutex<Vec<String>>,
    pub fail_apply: Mutex<bool>,
}

impl MockFileManager {
    pub fn new() -> Self {
        Self {
            scan_source_calls: Mutex::new(Vec::new()),
            scan_target_calls: Mutex::new(Vec::new()),
            diff_calls: Mutex::new(Vec::new()),
            apply_calls: Mutex::new(Vec::new()),
            fail_apply: Mutex::new(false),
        }
    }

    pub fn set_fail_apply(&self, fail: bool) {
        *self.fail_apply.lock().unwrap() = fail;
    }
}

impl Default for MockFileManager {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::providers::FileManager for MockFileManager {
    fn scan_source(&self, layers: &[FileLayer]) -> crate::errors::Result<FileTree> {
        let names: Vec<String> = layers.iter().map(|l| l.origin_source.clone()).collect();
        self.scan_source_calls.lock().unwrap().push(names.join(","));
        Ok(FileTree {
            files: BTreeMap::new(),
        })
    }

    fn scan_target(&self, paths: &[PathBuf]) -> crate::errors::Result<FileTree> {
        let names: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        self.scan_target_calls.lock().unwrap().push(names.join(","));
        Ok(FileTree {
            files: BTreeMap::new(),
        })
    }

    fn diff(&self, _source: &FileTree, _target: &FileTree) -> crate::errors::Result<Vec<FileDiff>> {
        self.diff_calls.lock().unwrap().push("diff".into());
        Ok(Vec::new())
    }

    fn apply(&self, actions: &[FileAction], _printer: &Printer) -> crate::errors::Result<()> {
        self.apply_calls
            .lock()
            .unwrap()
            .push(format!("{} actions", actions.len()));
        if *self.fail_apply.lock().unwrap() {
            return Err(CfgdError::File(FileError::SourceNotFound {
                path: PathBuf::from("mock-failure"),
            }));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// MockSecretBackend
// ---------------------------------------------------------------------------

/// Mock `SecretBackend` that tracks calls and returns configurable results.
pub struct MockSecretBackend {
    pub backend_name: String,
    pub available: bool,
    pub decrypt_calls: Mutex<Vec<PathBuf>>,
    pub encrypt_calls: Mutex<Vec<PathBuf>>,
    pub edit_calls: Mutex<Vec<PathBuf>>,
    pub decrypt_result: Mutex<Option<String>>,
    pub fail_decrypt: Mutex<bool>,
}

impl MockSecretBackend {
    pub fn new(name: &str) -> Self {
        Self {
            backend_name: name.to_string(),
            available: true,
            decrypt_calls: Mutex::new(Vec::new()),
            encrypt_calls: Mutex::new(Vec::new()),
            edit_calls: Mutex::new(Vec::new()),
            decrypt_result: Mutex::new(Some("mock-secret-value".into())),
            fail_decrypt: Mutex::new(false),
        }
    }

    pub fn unavailable(mut self) -> Self {
        self.available = false;
        self
    }

    pub fn with_decrypt_result(self, value: &str) -> Self {
        *self.decrypt_result.lock().unwrap() = Some(value.to_string());
        self
    }

    pub fn set_fail_decrypt(&self, fail: bool) {
        *self.fail_decrypt.lock().unwrap() = fail;
    }
}

impl SecretBackend for MockSecretBackend {
    fn name(&self) -> &str {
        &self.backend_name
    }

    fn is_available(&self) -> bool {
        self.available
    }

    fn encrypt_file(&self, path: &Path) -> crate::errors::Result<()> {
        self.encrypt_calls.lock().unwrap().push(path.to_path_buf());
        Ok(())
    }

    fn decrypt_file(&self, path: &Path) -> crate::errors::Result<SecretString> {
        self.decrypt_calls.lock().unwrap().push(path.to_path_buf());
        if *self.fail_decrypt.lock().unwrap() {
            return Err(CfgdError::Secret(SecretError::DecryptionFailed {
                path: path.to_path_buf(),
                message: "mock decrypt failure".into(),
            }));
        }
        let value = self
            .decrypt_result
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_default();
        Ok(SecretString::from(value))
    }

    fn edit_file(&self, path: &Path) -> crate::errors::Result<()> {
        self.edit_calls.lock().unwrap().push(path.to_path_buf());
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// MockSecretProvider
// ---------------------------------------------------------------------------

/// Mock `SecretProvider` that tracks resolve calls and returns configurable results.
pub struct MockSecretProvider {
    pub provider_name: String,
    pub available: bool,
    pub resolve_calls: Mutex<Vec<String>>,
    pub resolve_result: Mutex<Option<String>>,
    pub fail_resolve: Mutex<bool>,
}

impl MockSecretProvider {
    pub fn new(name: &str) -> Self {
        Self {
            provider_name: name.to_string(),
            available: true,
            resolve_calls: Mutex::new(Vec::new()),
            resolve_result: Mutex::new(Some("mock-resolved-secret".into())),
            fail_resolve: Mutex::new(false),
        }
    }

    pub fn unavailable(mut self) -> Self {
        self.available = false;
        self
    }

    pub fn with_resolve_result(self, value: &str) -> Self {
        *self.resolve_result.lock().unwrap() = Some(value.to_string());
        self
    }

    pub fn set_fail_resolve(&self, fail: bool) {
        *self.fail_resolve.lock().unwrap() = fail;
    }
}

impl SecretProvider for MockSecretProvider {
    fn name(&self) -> &str {
        &self.provider_name
    }

    fn is_available(&self) -> bool {
        self.available
    }

    fn resolve(&self, reference: &str) -> crate::errors::Result<SecretString> {
        self.resolve_calls
            .lock()
            .unwrap()
            .push(reference.to_string());
        if *self.fail_resolve.lock().unwrap() {
            return Err(CfgdError::Secret(SecretError::UnresolvableRef {
                reference: reference.to_string(),
            }));
        }
        let value = self
            .resolve_result
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_default();
        Ok(SecretString::from(value))
    }
}

// ---------------------------------------------------------------------------
// MockSystemConfigurator
// ---------------------------------------------------------------------------

/// Mock `SystemConfigurator` that returns configurable drift and records apply calls.
pub struct MockSystemConfigurator {
    pub configurator_name: String,
    pub available: bool,
    pub apply_calls: Mutex<Vec<serde_yaml::Value>>,
    pub drift: Mutex<Vec<SystemDrift>>,
    pub fail_apply: Mutex<bool>,
    pub fail_diff: Mutex<bool>,
}

impl MockSystemConfigurator {
    pub fn new(name: &str) -> Self {
        Self {
            configurator_name: name.to_string(),
            available: true,
            apply_calls: Mutex::new(Vec::new()),
            drift: Mutex::new(Vec::new()),
            fail_apply: Mutex::new(false),
            fail_diff: Mutex::new(false),
        }
    }

    pub fn unavailable(mut self) -> Self {
        self.available = false;
        self
    }

    pub fn with_drift(self, drifts: Vec<SystemDrift>) -> Self {
        *self.drift.lock().unwrap() = drifts;
        self
    }

    pub fn failing(self) -> Self {
        *self.fail_diff.lock().unwrap() = true;
        self
    }

    pub fn set_fail_apply(&self, fail: bool) {
        *self.fail_apply.lock().unwrap() = fail;
    }
}

impl SystemConfigurator for MockSystemConfigurator {
    fn name(&self) -> &str {
        &self.configurator_name
    }

    fn is_available(&self) -> bool {
        self.available
    }

    fn current_state(&self) -> crate::errors::Result<serde_yaml::Value> {
        Ok(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()))
    }

    fn diff(&self, _desired: &serde_yaml::Value) -> crate::errors::Result<Vec<SystemDrift>> {
        if *self.fail_diff.lock().unwrap() {
            return Err(CfgdError::Io(std::io::Error::other("mock diff failed")));
        }
        let items = self.drift.lock().unwrap();
        Ok(items
            .iter()
            .map(|d| SystemDrift {
                key: d.key.clone(),
                expected: d.expected.clone(),
                actual: d.actual.clone(),
            })
            .collect())
    }

    fn apply(&self, desired: &serde_yaml::Value, _printer: &Printer) -> crate::errors::Result<()> {
        self.apply_calls.lock().unwrap().push(desired.clone());
        if *self.fail_apply.lock().unwrap() {
            return Err(CfgdError::Io(std::io::Error::other(
                "mock system apply failure",
            )));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// TestEnvBuilder
// ---------------------------------------------------------------------------

/// Fluent builder for creating isolated test environments with temp directories,
/// config files, profiles, modules, and arbitrary files.
pub struct TestEnvBuilder {
    /// Root temp directory (created on `build()`).
    dir: Option<tempfile::TempDir>,
    configs: Vec<(String, String)>,
    profiles: Vec<(String, String)>,
    modules: Vec<(String, String)>,
    files: Vec<(String, String)>,
}

impl TestEnvBuilder {
    pub fn new() -> Self {
        Self {
            dir: None,
            configs: Vec::new(),
            profiles: Vec::new(),
            modules: Vec::new(),
            files: Vec::new(),
        }
    }

    /// Add a config file. `name` is relative to the config dir (e.g. `"cfgd.yaml"`).
    pub fn config(mut self, name: &str, content: &str) -> Self {
        self.configs.push((name.to_string(), content.to_string()));
        self
    }

    /// Add a profile file. `name` is relative to the profiles dir (e.g. `"default.yaml"`).
    pub fn profile(mut self, name: &str, content: &str) -> Self {
        self.profiles.push((name.to_string(), content.to_string()));
        self
    }

    /// Add a module file. `name` is relative to the modules dir (e.g. `"nvim/module.yaml"`).
    pub fn module(mut self, name: &str, content: &str) -> Self {
        self.modules.push((name.to_string(), content.to_string()));
        self
    }

    /// Add an arbitrary file. `path` is relative to the temp root.
    pub fn file(mut self, path: &str, content: &str) -> Self {
        self.files.push((path.to_string(), content.to_string()));
        self
    }

    /// Build the test environment. Returns a `TestEnv` that owns the temp directory.
    pub fn build(mut self) -> TestEnv {
        let dir = tempfile::TempDir::new().expect("failed to create temp dir");
        let root = dir.path().to_path_buf();

        let config_dir = root.join("config");
        let profiles_dir = root.join("profiles");
        let modules_dir = root.join("modules");
        let state_dir = root.join("state");

        std::fs::create_dir_all(&config_dir).expect("create config dir");
        std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
        std::fs::create_dir_all(&modules_dir).expect("create modules dir");
        std::fs::create_dir_all(&state_dir).expect("create state dir");

        for (name, content) in &self.configs {
            let path = config_dir.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create config subdirs");
            }
            std::fs::write(&path, content).expect("write config file");
        }

        for (name, content) in &self.profiles {
            let path = profiles_dir.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create profile subdirs");
            }
            std::fs::write(&path, content).expect("write profile file");
        }

        for (name, content) in &self.modules {
            let path = modules_dir.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create module subdirs");
            }
            std::fs::write(&path, content).expect("write module file");
        }

        for (rel_path, content) in &self.files {
            let path = root.join(rel_path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create file subdirs");
            }
            std::fs::write(&path, content).expect("write file");
        }

        // Install a thread-local HOME override pointing at the tempdir root.
        // Any code path that later calls `expand_tilde("~")` or
        // `default_config_dir()` on this thread resolves into this tempdir
        // instead of the real user home. The guard is dropped with the
        // TestEnv, restoring the prior override (or clearing it).
        let home_guard = crate::with_test_home_guard(&root);

        self.dir = Some(dir);

        TestEnv {
            _home_guard: home_guard,
            _dir: self.dir.take().unwrap(),
            root,
            config_dir,
            profiles_dir,
            modules_dir,
            state_dir,
        }
    }
}

impl Default for TestEnvBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// An isolated test environment backed by a temp directory.
///
/// Dropping this struct restores the previous thread-local HOME override AND
/// removes all files (in that order — see field ordering below).
///
/// Field drop order matters: Rust drops struct fields in declaration order,
/// so `_home_guard` is declared first to run BEFORE `_dir`. That way any
/// code executed during the tempdir's teardown (e.g. a Drop impl somewhere
/// that resolves `~`) sees the real `$HOME` rather than a dangling override
/// pointing at a just-deleted path.
pub struct TestEnv {
    /// Restores the prior thread-local HOME override on drop.
    _home_guard: crate::TestHomeGuard,
    /// Owns the tempdir — deleted last, after the guard is released.
    _dir: tempfile::TempDir,
    pub root: PathBuf,
    pub config_dir: PathBuf,
    pub profiles_dir: PathBuf,
    pub modules_dir: PathBuf,
    pub state_dir: PathBuf,
}

impl TestEnv {
    /// Convenience: write an additional file after build.
    pub fn write_file(&self, rel_path: &str, content: &str) {
        let path = self.root.join(rel_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dirs");
        }
        std::fs::write(&path, content).expect("write file");
    }

    /// Read a file relative to the test root.
    ///
    /// Named `read_at` (not `read_file`) to avoid a workspace-wide name
    /// collision with production `cfgd::generate::files::read_file` — the
    /// DRY audit flags same-named functions across files, and the two serve
    /// different purposes (test-env helper vs. path-traversal-validated
    /// reader).
    pub fn read_at(&self, rel_path: &str) -> String {
        std::fs::read_to_string(self.root.join(rel_path)).expect("read file")
    }

    /// Check if a file exists relative to the root.
    pub fn file_exists(&self, rel_path: &str) -> bool {
        self.root.join(rel_path).exists()
    }

    /// Full path for a relative path.
    pub fn path(&self, rel_path: &str) -> PathBuf {
        self.root.join(rel_path)
    }
}

// ---------------------------------------------------------------------------
// init_test_git_repo
// ---------------------------------------------------------------------------

/// Initialize a minimal git repository at `dir` with an initial commit.
/// Useful for tests that depend on git operations (sources, modules, etc.).
pub fn init_test_git_repo(dir: &Path) {
    std::fs::create_dir_all(dir).expect("create git repo dir");

    let repo = git2::Repository::init(dir).expect("git init");

    // Configure committer identity for the test repo
    let mut config = repo.config().expect("repo config");
    config
        .set_str("user.name", "cfgd-test")
        .expect("set user.name");
    config
        .set_str("user.email", "test@cfgd.io")
        .expect("set user.email");

    // Create a minimal file and commit it
    let readme_path = dir.join("README");
    std::fs::write(&readme_path, "test repo\n").expect("write README");

    let mut index = repo.index().expect("repo index");
    index
        .add_path(Path::new("README"))
        .expect("add README to index");
    index.write().expect("write index");

    let tree_id = index.write_tree().expect("write tree");
    let tree = repo.find_tree(tree_id).expect("find tree");

    let sig = repo.signature().expect("signature");
    repo.commit(Some("HEAD"), &sig, &sig, "initial commit", &tree, &[])
        .expect("initial commit");
}

// ---------------------------------------------------------------------------
// Printer helper
// ---------------------------------------------------------------------------

/// Create a quiet `Printer` for tests that exercise the reconciler entry
/// surface (`Reconciler::apply`, `Reconciler::apply_action`, and per-action
/// helpers in `apply.rs` / `modules.rs` / `packages.rs` / `secrets.rs` /
/// `system.rs`) plus mock trait impls (`MockPackageManager`,
/// `MockSecretBackend`, `MockSystemConfigurator`).
///
/// Returns a bare `Printer` (not the `(Printer, Buffer)` tuple from
/// `Printer::for_test()`) so it drops in as a direct replacement in fixtures
/// that don't assert on captured output.
pub fn test_printer_v2() -> crate::output::Printer {
    crate::output::Printer::new(crate::output::Verbosity::Quiet)
}

// ---------------------------------------------------------------------------
// FileStrategy re-export for convenience
// ---------------------------------------------------------------------------

pub use crate::config::FileStrategy as TestFileStrategy;

// ---------------------------------------------------------------------------
// Platform helpers
// ---------------------------------------------------------------------------

/// A Linux/Ubuntu/x86_64 platform — the most common test platform.
pub fn linux_ubuntu_platform() -> crate::platform::Platform {
    crate::platform::Platform {
        os: crate::platform::Os::Linux,
        distro: crate::platform::Distro::Ubuntu,
        version: "22.04".into(),
        arch: crate::platform::Arch::X86_64,
    }
}

/// A macOS/Aarch64 platform for macOS-specific test paths.
pub fn macos_platform() -> crate::platform::Platform {
    crate::platform::Platform {
        os: crate::platform::Os::MacOS,
        distro: crate::platform::Distro::MacOS,
        version: "14.0".into(),
        arch: crate::platform::Arch::Aarch64,
    }
}

// ---------------------------------------------------------------------------
// Profile / resolved-profile helpers
// ---------------------------------------------------------------------------

/// Minimal `ResolvedProfile` with a single local layer and empty merged profile.
/// The workhorse of reconciler and module tests — used as the baseline resolved state.
pub fn make_empty_resolved() -> crate::config::ResolvedProfile {
    crate::config::ResolvedProfile {
        layers: vec![crate::config::ProfileLayer {
            source: "local".to_string(),
            profile_name: "test".to_string(),
            priority: 1000,
            policy: crate::config::LayerPolicy::Local,
            spec: crate::config::ProfileSpec::default(),
        }],
        merged: crate::config::MergedProfile::default(),
    }
}

// ---------------------------------------------------------------------------
// State helpers
// ---------------------------------------------------------------------------

/// Open an in-memory `StateStore` for tests. Panics on failure.
pub fn test_state() -> crate::state::StateStore {
    crate::state::StateStore::open_in_memory().expect("open in-memory state store")
}

// ---------------------------------------------------------------------------
// Module helpers
// ---------------------------------------------------------------------------

/// Build a `ResolvedModule` with sample packages and sensible defaults.
/// Useful for reconciler tests that need a module with real package actions.
pub fn make_resolved_module(name: &str) -> crate::modules::ResolvedModule {
    crate::modules::ResolvedModule {
        name: name.to_string(),
        packages: vec![
            crate::modules::ResolvedPackage {
                canonical_name: "neovim".to_string(),
                resolved_name: "neovim".to_string(),
                manager: "brew".to_string(),
                version: Some("0.10.2".to_string()),
                script: None,
            },
            crate::modules::ResolvedPackage {
                canonical_name: "ripgrep".to_string(),
                resolved_name: "ripgrep".to_string(),
                manager: "brew".to_string(),
                version: Some("14.1.0".to_string()),
                script: None,
            },
        ],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        system: std::collections::HashMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
    }
}

/// Build a map of `(name, deps)` tuples into `LoadedModule`s for dependency resolution tests.
pub fn make_test_modules(
    specs: &[(&str, &[&str])],
) -> std::collections::HashMap<String, crate::modules::LoadedModule> {
    let mut modules = std::collections::HashMap::new();
    for (name, deps) in specs {
        modules.insert(
            name.to_string(),
            crate::modules::LoadedModule {
                name: name.to_string(),
                spec: crate::config::ModuleSpec {
                    depends: deps.iter().map(|s| s.to_string()).collect(),
                    ..Default::default()
                },
                dir: PathBuf::from(format!("/fake/{name}")),
            },
        );
    }
    modules
}

/// Build a package-manager lookup map from `(name, &dyn PackageManager)` slices.
pub fn make_manager_map<'a>(
    entries: &[(&str, &'a dyn crate::providers::PackageManager)],
) -> std::collections::HashMap<String, &'a dyn crate::providers::PackageManager> {
    entries
        .iter()
        .map(|(name, mgr)| (name.to_string(), *mgr))
        .collect()
}

// ---------------------------------------------------------------------------
// YAML fixture constants
// ---------------------------------------------------------------------------

/// A minimal cfgd config with a git origin.
pub const SAMPLE_CONFIG_YAML: &str = r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test-config
spec:
  profile: default
  origin:
    type: Git
    url: https://github.com/test/repo.git
    branch: master
"#;

/// A minimal cfgd config without any origin.
pub const SAMPLE_CONFIG_NO_ORIGIN_YAML: &str = r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test-config
spec:
  profile: default
"#;

/// A base profile with env vars and packages.
pub const SAMPLE_PROFILE_YAML: &str = r#"
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: base
spec:
  env:
    - name: editor
      value: vim
    - name: shell
      value: /bin/zsh
  packages:
    brew:
      formulae:
        - ripgrep
        - fd
    cargo:
      - bat
"#;

/// A minimal module YAML for the "nvim" module.
pub const SAMPLE_MODULE_YAML: &str = r#"
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: nvim
spec:
  depends: [node]
  packages:
    - name: neovim
      minVersion: "0.9"
      prefer: [brew, snap, apt]
      aliases:
        snap: nvim
    - name: ripgrep
  files:
    - source: config/
      target: ~/.config/nvim/
"#;

// ---------------------------------------------------------------------------
// External-CLI shim — used by every backend that shells out to a tool, to
// exercise the spawn/exit/stderr code paths without requiring the real binary
// installed on the runner. Pair with `serial_test::serial` because env-var
// mutation is process-global.
// ---------------------------------------------------------------------------

/// Owns a tempdir holding a `/bin/sh` shim binary plus the env-vars that
/// route a single `tool_cmd(env_var, default)` factory at it. The shim
/// records its full argv to a log file and exits with a chosen status,
/// optionally writing canned stdout/stderr.
///
/// Construct with [`ToolShim::install`]. Drops the env-vars and tempdir on
/// drop, even when a test panics — env state never leaks across tests.
///
/// Unix-only: the shim is a `/bin/sh` script. Tests using this helper should
/// be gated behind `#[cfg(unix)]`.
#[cfg(unix)]
pub struct ToolShim {
    _tmp: tempfile::TempDir,
    env_var: String,
    log_path: std::path::PathBuf,
}

#[cfg(unix)]
impl ToolShim {
    /// Install a shim that records argv to a log and exits with `exit_code`,
    /// emitting `stdout` to stdout and `stderr` to stderr. The shim is pointed
    /// at by the `env_var` env-var (the same var read by `tool_cmd`).
    ///
    /// Implementation detail: `CFGD_TOOL_SHIM_LOG` is read by the shim script
    /// itself (per-test path, no cross-test collision) and `argv` is appended
    /// one line per invocation so multi-call tests can assert ordering.
    pub fn install(env_var: &str, exit_code: i32, stdout: &str, stderr: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let bin_path = tmp.path().join(format!("shim-{env_var}"));
        let log_path = tmp.path().join("argv.log");

        // Single-quote-safe escaping: replace ' with '\''.
        let stdout_lit = stdout.replace('\'', "'\\''");
        let stderr_lit = stderr.replace('\'', "'\\''");

        let script = format!(
            "#!/bin/sh\n\
             printf '%s\\n' \"$*\" >> \"$CFGD_TOOL_SHIM_LOG\"\n\
             printf '%s' '{stdout_lit}'\n\
             printf '%s' '{stderr_lit}' 1>&2\n\
             exit {exit_code}\n",
        );
        std::fs::write(&bin_path, script).expect("write shim");
        let mut perms = std::fs::metadata(&bin_path).expect("stat").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin_path, perms).expect("chmod");

        // SAFETY: callers wrap with `serial_test::serial`, so no concurrent
        // reader observes a mid-update env state.
        unsafe {
            std::env::set_var(env_var, &bin_path);
            std::env::set_var("CFGD_TOOL_SHIM_LOG", &log_path);
        }

        Self {
            _tmp: tmp,
            env_var: env_var.to_string(),
            log_path,
        }
    }

    /// Read the captured argv. Each line is the space-joined argv of one
    /// invocation, in order.
    pub fn argv_log(&self) -> String {
        std::fs::read_to_string(&self.log_path).unwrap_or_default()
    }

    /// Number of times the shim was invoked.
    pub fn invocation_count(&self) -> usize {
        self.argv_log().lines().filter(|l| !l.is_empty()).count()
    }
}

#[cfg(unix)]
impl Drop for ToolShim {
    fn drop(&mut self) {
        // SAFETY: see `install`.
        unsafe {
            std::env::remove_var(&self.env_var);
            std::env::remove_var("CFGD_TOOL_SHIM_LOG");
        }
    }
}

// ---------------------------------------------------------------------------
// Env-var test guards — replace per-file `struct EnvVarGuard` / `fn with_env`
// duplicates. Pair with `serial_test::serial` because env-var mutation is
// process-global. The pattern mirrors `cfgd-core/src/util/git.rs:190`.
// ---------------------------------------------------------------------------

/// RAII guard that captures the prior value of an env var and restores it on
/// drop (or removes the var if no prior value existed). Use in tests that
/// mutate process-global env state.
pub struct EnvVarGuard {
    key: &'static str,
    prior: Option<String>,
}

impl EnvVarGuard {
    /// Capture the prior value of `key`, then set it to `value`.
    pub fn set(key: &'static str, value: &str) -> Self {
        let prior = std::env::var(key).ok();
        // SAFETY: serial_test::serial gates execution; no concurrent reader/writer.
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, prior }
    }

    /// Capture the prior value of `key`, then remove it.
    pub fn unset(key: &'static str) -> Self {
        let prior = std::env::var(key).ok();
        // SAFETY: serial_test::serial gates execution; no concurrent reader/writer.
        unsafe {
            std::env::remove_var(key);
        }
        Self { key, prior }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // SAFETY: serial_test::serial gates execution; no concurrent reader/writer.
        unsafe {
            match self.prior.take() {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

/// RAII guard that sets `EDITOR` for the duration of the closure. Pair with
/// `#[serial]` so concurrent tests don't observe the override. Use in tests
/// that drive `open_in_editor` / `open_in_editor` against a known editor
/// binary (e.g. `/bin/true` for no-op, `/bin/sh -c '...'` for content
/// rewrites). `unsafe` is sound under `#[serial]` since env mutation is
/// process-global and the guard preserves the prior value across panics.
pub struct EditorGuard {
    prior: Option<String>,
}

impl EditorGuard {
    /// Capture the prior value of `EDITOR`, then set it to `editor`.
    pub fn set(editor: &str) -> Self {
        // SAFETY: serial_test::serial gates execution; no concurrent reader/writer.
        let prior = std::env::var("EDITOR").ok();
        unsafe {
            std::env::set_var("EDITOR", editor);
        }
        Self { prior }
    }
}

impl Drop for EditorGuard {
    fn drop(&mut self) {
        // SAFETY: serial_test::serial gates execution; no concurrent reader/writer.
        unsafe {
            match self.prior.take() {
                Some(v) => std::env::set_var("EDITOR", v),
                None => std::env::remove_var("EDITOR"),
            }
        }
    }
}

/// Function-call style env-var scope: capture prior, set/unset `var` to
/// `value`, run `f`, then restore. `value = None` removes the var for the
/// duration of `f`.
pub fn with_test_env_var<F: FnOnce()>(var: &str, value: Option<&str>, f: F) {
    // SAFETY: serial_test::serial gates execution; no concurrent reader/writer.
    unsafe {
        let prior = std::env::var(var).ok();
        match value {
            Some(v) => std::env::set_var(var, v),
            None => std::env::remove_var(var),
        }
        f();
        match prior {
            Some(v) => std::env::set_var(var, v),
            None => std::env::remove_var(var),
        }
    }
}

// ---------------------------------------------------------------------------
// CosignTestShim — consolidated fake-cosign shim for the three legacy variants
// in `oci/sign/tests.rs` (CosignShimGuard), `upgrade/tests.rs` (CosignShim),
// and `cli/module/tests.rs` (CosignKeygenShim). Builder configures argv
// logging, keygen mode, exit code, and stderr in one place so consumers can
// collapse to one type. Pair with `serial_test::serial` — env-var mutation
// is process-global. Unix-only: shim is a `/bin/sh` script.
// ---------------------------------------------------------------------------
//
// Env vars owned by this shim:
// - CFGD_COSIGN_BIN          — points cosign_cmd() at the shim binary
// - CFGD_FAKE_COSIGN_LOG     — per-shim argv log path (only set when argv
//                              logging is enabled; matches the legacy log
//                              env var so consumer migration is mechanical).
//
// Prior values for both vars are captured on `install()` and restored on
// drop, even if the test panics.

/// Builder + RAII guard for a fake `cosign` binary. Configure with the
/// `with_*` methods, then call [`CosignTestShim::install`] to write the
/// script and set `CFGD_COSIGN_BIN`. Drops the env vars + tempdir when
/// the returned value goes out of scope.
///
/// Unix-only: the shim is a `/bin/sh` script. Gate consumers behind
/// `#[cfg(unix)]`.
#[cfg(unix)]
pub struct CosignTestShim {
    _tmp: tempfile::TempDir,
    log_path: std::path::PathBuf,
    argv_logging: bool,
    prior_bin: Option<String>,
    prior_log: Option<String>,
}

#[cfg(unix)]
impl CosignTestShim {
    /// Builder entry point. Chain `with_*` methods then call [`install`].
    pub fn builder() -> CosignTestShimBuilder {
        CosignTestShimBuilder::default()
    }

    /// Install with defaults: argv logging on, keygen off, exit 0, empty
    /// stderr. Equivalent to `CosignTestShim::builder().install()`.
    pub fn install() -> Self {
        Self::builder().install()
    }

    /// Read the captured argv log. Each line is the space-joined argv of
    /// one invocation, in order. Returns empty string if argv logging is
    /// disabled or the shim was never invoked.
    pub fn argv_log(&self) -> String {
        if !self.argv_logging {
            return String::new();
        }
        std::fs::read_to_string(&self.log_path).unwrap_or_default()
    }

    /// Number of times the shim was invoked. Returns 0 if argv logging is
    /// disabled.
    pub fn invocation_count(&self) -> usize {
        self.argv_log().lines().filter(|l| !l.is_empty()).count()
    }
}

#[cfg(unix)]
impl Drop for CosignTestShim {
    fn drop(&mut self) {
        // SAFETY: callers wrap with `serial_test::serial`, so no concurrent
        // reader observes a mid-update env state.
        unsafe {
            match self.prior_bin.take() {
                Some(v) => std::env::set_var("CFGD_COSIGN_BIN", v),
                None => std::env::remove_var("CFGD_COSIGN_BIN"),
            }
            match self.prior_log.take() {
                Some(v) => std::env::set_var("CFGD_FAKE_COSIGN_LOG", v),
                None => std::env::remove_var("CFGD_FAKE_COSIGN_LOG"),
            }
        }
    }
}

/// Builder for [`CosignTestShim`]. All fields default to the most common
/// existing variant: argv logging on, keygen off, exit 0, empty stderr.
#[cfg(unix)]
pub struct CosignTestShimBuilder {
    argv_logging: bool,
    keygen: bool,
    exit_code: i32,
    stderr: String,
}

#[cfg(unix)]
impl Default for CosignTestShimBuilder {
    fn default() -> Self {
        Self {
            argv_logging: true,
            keygen: false,
            exit_code: 0,
            stderr: String::new(),
        }
    }
}

#[cfg(unix)]
impl CosignTestShimBuilder {
    /// Enable or disable argv logging. When enabled, every invocation
    /// appends one space-joined-argv line to the log file, readable via
    /// `CosignTestShim::argv_log()`. Default: enabled.
    pub fn with_argv_logging(mut self, enabled: bool) -> Self {
        self.argv_logging = enabled;
        self
    }

    /// Enable keygen mode. When enabled, invoking the shim with
    /// `generate-key-pair` as `$1` writes `cosign.key` and `cosign.pub`
    /// to the current working directory (matching real cosign behavior).
    /// Default: disabled.
    pub fn with_keygen(mut self, enabled: bool) -> Self {
        self.keygen = enabled;
        self
    }

    /// Set the shim's exit code. Default: 0.
    pub fn with_exit(mut self, code: i32) -> Self {
        self.exit_code = code;
        self
    }

    /// Set the stderr the shim emits on every invocation. Default: empty.
    pub fn with_stderr(mut self, stderr: &str) -> Self {
        self.stderr = stderr.to_string();
        self
    }

    /// Write the shim script to a tempfile, set `CFGD_COSIGN_BIN` (and,
    /// when argv logging is on, `CFGD_FAKE_COSIGN_LOG`). Prior values for
    /// both vars are captured for restoration on drop.
    pub fn install(self) -> CosignTestShim {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let bin_path = tmp.path().join("fake-cosign");
        let log_path = tmp.path().join("argv.log");

        // Single-quote-safe escape — replace ' with '\''.
        let stderr_lit = self.stderr.replace('\'', "'\\''");

        // Build the script in pieces so flags compose cleanly.
        let log_line = if self.argv_logging {
            "printf '%s\\n' \"$*\" >> \"$CFGD_FAKE_COSIGN_LOG\"\n"
        } else {
            ""
        };
        let keygen_block = if self.keygen {
            "if [ \"$1\" = \"generate-key-pair\" ]; then\n  printf 'fake-private-key-bytes' > cosign.key\n  printf 'fake-public-key-bytes' > cosign.pub\nfi\n"
        } else {
            ""
        };
        let script = format!(
            "#!/bin/sh\n{log_line}{keygen_block}printf '%s' '{stderr_lit}' 1>&2\nexit {exit}\n",
            exit = self.exit_code,
        );
        std::fs::write(&bin_path, script).expect("write fake-cosign");
        let mut perms = std::fs::metadata(&bin_path).expect("stat").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin_path, perms).expect("chmod");

        // Capture prior values for restoration on drop.
        let prior_bin = std::env::var("CFGD_COSIGN_BIN").ok();
        let prior_log = std::env::var("CFGD_FAKE_COSIGN_LOG").ok();

        // SAFETY: callers wrap with `serial_test::serial`, so no concurrent
        // reader observes a mid-update env state.
        unsafe {
            std::env::set_var("CFGD_COSIGN_BIN", &bin_path);
            if self.argv_logging {
                std::env::set_var("CFGD_FAKE_COSIGN_LOG", &log_path);
            } else {
                std::env::remove_var("CFGD_FAKE_COSIGN_LOG");
            }
        }

        CosignTestShim {
            _tmp: tmp,
            log_path,
            argv_logging: self.argv_logging,
            prior_bin,
            prior_log,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::FileManager;
    use secrecy::ExposeSecret;

    #[test]
    fn mock_file_manager_records_calls() {
        let fm = MockFileManager::new();
        let layers = vec![FileLayer {
            source_dir: PathBuf::from("/tmp/src"),
            origin_source: "test-origin".into(),
            priority: 0,
        }];
        let printer = test_printer_v2();

        let tree = fm.scan_source(&layers).unwrap();
        assert!(tree.files.is_empty());
        assert_eq!(fm.scan_source_calls.lock().unwrap().len(), 1);

        let target_tree = fm.scan_target(&[PathBuf::from("/tmp/target")]).unwrap();
        assert!(target_tree.files.is_empty());
        assert_eq!(fm.scan_target_calls.lock().unwrap().len(), 1);

        let diffs = fm.diff(&tree, &target_tree).unwrap();
        assert!(diffs.is_empty());

        fm.apply(&[], &printer).unwrap();
        assert_eq!(fm.apply_calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn mock_file_manager_can_fail() {
        let fm = MockFileManager::new();
        let printer = test_printer_v2();
        fm.set_fail_apply(true);
        let result = fm.apply(&[], &printer);
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("mock-failure"),
            "expected mock-failure path in error, got: {err_msg}"
        );
    }

    #[test]
    fn mock_secret_backend_tracks_decrypt() {
        let backend = MockSecretBackend::new("sops");
        let secret = backend.decrypt_file(Path::new("/tmp/secret.enc")).unwrap();
        assert_eq!(secret.expose_secret(), "mock-secret-value");
        assert_eq!(backend.decrypt_calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn mock_secret_backend_can_fail() {
        let backend = MockSecretBackend::new("sops");
        backend.set_fail_decrypt(true);
        let result = backend.decrypt_file(Path::new("/tmp/secret.enc"));
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("mock decrypt failure"),
            "expected 'mock decrypt failure' in error, got: {err_msg}"
        );
    }

    #[test]
    fn mock_secret_provider_resolve() {
        let provider = MockSecretProvider::new("1password");
        let secret = provider.resolve("vault/item/field").unwrap();
        assert_eq!(secret.expose_secret(), "mock-resolved-secret");
        assert_eq!(provider.resolve_calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn mock_secret_provider_can_fail() {
        let provider = MockSecretProvider::new("1password");
        provider.set_fail_resolve(true);
        let result = provider.resolve("vault/item/field");
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("vault/item/field"),
            "expected reference in error, got: {err_msg}"
        );
    }

    #[test]
    fn mock_system_configurator_empty_state() {
        let sc = MockSystemConfigurator::new("sysctl");
        let state = sc.current_state().unwrap();
        assert!(state.as_mapping().unwrap().is_empty());
    }

    #[test]
    fn mock_system_configurator_with_drift() {
        let sc = MockSystemConfigurator::new("sysctl").with_drift(vec![SystemDrift {
            key: "net.ipv4.ip_forward".into(),
            expected: "1".into(),
            actual: "0".into(),
        }]);
        let desired = serde_yaml::Value::Null;
        let drifts = sc.diff(&desired).unwrap();
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].key, "net.ipv4.ip_forward");
    }

    #[test]
    fn mock_system_configurator_apply_records() {
        let sc = MockSystemConfigurator::new("sysctl");
        let printer = test_printer_v2();
        let desired = serde_yaml::Value::String("test".into());
        sc.apply(&desired, &printer).unwrap();
        assert_eq!(sc.apply_calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn mock_system_configurator_can_fail() {
        let sc = MockSystemConfigurator::new("sysctl");
        let printer = test_printer_v2();
        sc.set_fail_apply(true);
        let result = sc.apply(&serde_yaml::Value::Null, &printer);
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("mock system apply failure"),
            "expected 'mock system apply failure' in error, got: {err_msg}"
        );
    }

    #[test]
    fn test_env_builder_creates_dirs() {
        let env = TestEnvBuilder::new()
            .config("cfgd.yaml", "apiVersion: cfgd.io/v1alpha1\n")
            .profile("default.yaml", "kind: Profile\n")
            .module("nvim/module.yaml", "kind: Module\n")
            .file("extra/data.txt", "hello\n")
            .build();

        assert!(env.config_dir.exists());
        assert!(env.profiles_dir.exists());
        assert!(env.modules_dir.exists());
        assert!(env.state_dir.exists());
        assert!(env.file_exists("config/cfgd.yaml"));
        assert!(env.file_exists("profiles/default.yaml"));
        assert!(env.file_exists("modules/nvim/module.yaml"));
        assert!(env.file_exists("extra/data.txt"));
        assert_eq!(env.read_at("extra/data.txt"), "hello\n");
    }

    #[test]
    fn test_env_write_after_build() {
        let env = TestEnvBuilder::new().build();
        assert!(!env.file_exists("late.txt"));
        env.write_file("late.txt", "added later");
        assert!(env.file_exists("late.txt"));
        assert_eq!(env.read_at("late.txt"), "added later");
    }

    #[test]
    fn init_test_git_repo_creates_valid_repo() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo_dir = tmp.path().join("repo");
        init_test_git_repo(&repo_dir);

        let repo = git2::Repository::open(&repo_dir).unwrap();
        let head = repo.head().unwrap();
        assert!(head.is_branch());

        let commit = head.peel_to_commit().unwrap();
        assert_eq!(commit.message().unwrap(), "initial commit");
    }

    // -----------------------------------------------------------------------
    // EnvVarGuard / with_test_env_var
    // -----------------------------------------------------------------------

    use serial_test::serial;

    #[test]
    #[serial]
    fn env_var_guard_set_captures_prior_and_restores_on_drop() {
        const KEY: &str = "CFGD_TEST_GUARD_SET_1";
        // SAFETY: serial gates env mutation across tests.
        unsafe {
            std::env::set_var(KEY, "original");
        }

        {
            let _g = EnvVarGuard::set(KEY, "overridden");
            assert_eq!(std::env::var(KEY).ok().as_deref(), Some("overridden"));
        }

        assert_eq!(std::env::var(KEY).ok().as_deref(), Some("original"));
        // SAFETY: serial gates env mutation across tests.
        unsafe {
            std::env::remove_var(KEY);
        }
    }

    #[test]
    #[serial]
    fn env_var_guard_set_with_no_prior_removes_on_drop() {
        const KEY: &str = "CFGD_TEST_GUARD_SET_2";
        // SAFETY: serial gates env mutation across tests.
        unsafe {
            std::env::remove_var(KEY);
        }
        assert!(std::env::var(KEY).is_err());

        {
            let _g = EnvVarGuard::set(KEY, "value");
            assert_eq!(std::env::var(KEY).ok().as_deref(), Some("value"));
        }

        assert!(std::env::var(KEY).is_err());
    }

    #[test]
    #[serial]
    fn env_var_guard_unset_removes_and_restores_on_drop() {
        const KEY: &str = "CFGD_TEST_GUARD_UNSET_1";
        // SAFETY: serial gates env mutation across tests.
        unsafe {
            std::env::set_var(KEY, "before");
        }

        {
            let _g = EnvVarGuard::unset(KEY);
            assert!(std::env::var(KEY).is_err());
        }

        assert_eq!(std::env::var(KEY).ok().as_deref(), Some("before"));
        // SAFETY: serial gates env mutation across tests.
        unsafe {
            std::env::remove_var(KEY);
        }
    }

    #[test]
    #[serial]
    fn with_test_env_var_some_sets_and_restores() {
        const KEY: &str = "CFGD_TEST_WITH_ENV_SOME_1";
        // SAFETY: serial gates env mutation across tests.
        unsafe {
            std::env::set_var(KEY, "outer");
        }

        let mut observed = None;
        with_test_env_var(KEY, Some("inner"), || {
            observed = std::env::var(KEY).ok();
        });

        assert_eq!(observed.as_deref(), Some("inner"));
        assert_eq!(std::env::var(KEY).ok().as_deref(), Some("outer"));
        // SAFETY: serial gates env mutation across tests.
        unsafe {
            std::env::remove_var(KEY);
        }
    }

    #[test]
    #[serial]
    fn with_test_env_var_none_removes_and_restores() {
        const KEY: &str = "CFGD_TEST_WITH_ENV_NONE_1";
        // SAFETY: serial gates env mutation across tests.
        unsafe {
            std::env::set_var(KEY, "outer");
        }

        let mut observed_present = true;
        with_test_env_var(KEY, None, || {
            observed_present = std::env::var(KEY).is_ok();
        });

        assert!(!observed_present);
        assert_eq!(std::env::var(KEY).ok().as_deref(), Some("outer"));
        // SAFETY: serial gates env mutation across tests.
        unsafe {
            std::env::remove_var(KEY);
        }
    }

    #[test]
    #[serial]
    fn env_var_guard_round_trips_special_chars() {
        const KEY: &str = "CFGD_TEST_GUARD_SPECIAL_1";
        let weird = "a=b c\t\"quoted\" 'single' = trailing  ";
        // SAFETY: serial gates env mutation across tests.
        unsafe {
            std::env::set_var(KEY, weird);
        }

        {
            let _g = EnvVarGuard::set(KEY, "temp");
            assert_eq!(std::env::var(KEY).ok().as_deref(), Some("temp"));
        }

        assert_eq!(std::env::var(KEY).ok().as_deref(), Some(weird));
        // SAFETY: serial gates env mutation across tests.
        unsafe {
            std::env::remove_var(KEY);
        }
    }

    // -----------------------------------------------------------------------
    // CosignTestShim
    // -----------------------------------------------------------------------

    #[cfg(unix)]
    mod cosign_shim_tests {
        use super::super::CosignTestShim;
        use serial_test::serial;

        /// Run the installed shim with the given argv. Returns (exit_code,
        /// stderr_string). Reads $CFGD_COSIGN_BIN like real consumers.
        fn run_shim(args: &[&str]) -> (i32, String) {
            let bin = std::env::var("CFGD_COSIGN_BIN").expect("CFGD_COSIGN_BIN set");
            let output = std::process::Command::new(&bin)
                .args(args)
                .output()
                .expect("spawn shim");
            (
                output.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&output.stderr).into_owned(),
            )
        }

        #[test]
        #[serial]
        fn install_sets_cosign_bin_and_drop_restores_prior() {
            // SAFETY: serial gates env mutation across tests.
            unsafe {
                std::env::set_var("CFGD_COSIGN_BIN", "/prior/value");
            }

            {
                let _shim = CosignTestShim::install();
                let observed =
                    std::env::var("CFGD_COSIGN_BIN").expect("install sets CFGD_COSIGN_BIN");
                assert_ne!(observed, "/prior/value", "shim must override prior value");
                assert!(
                    std::path::Path::new(&observed).is_file(),
                    "CFGD_COSIGN_BIN must point at the shim file"
                );
            }

            assert_eq!(
                std::env::var("CFGD_COSIGN_BIN").ok().as_deref(),
                Some("/prior/value"),
                "drop must restore the prior value"
            );

            // SAFETY: serial gates env mutation across tests.
            unsafe {
                std::env::remove_var("CFGD_COSIGN_BIN");
            }
        }

        #[test]
        #[serial]
        fn install_with_no_prior_value_removes_on_drop() {
            // SAFETY: serial gates env mutation across tests.
            unsafe {
                std::env::remove_var("CFGD_COSIGN_BIN");
            }
            assert!(std::env::var("CFGD_COSIGN_BIN").is_err());

            {
                let _shim = CosignTestShim::install();
                assert!(std::env::var("CFGD_COSIGN_BIN").is_ok());
            }

            assert!(
                std::env::var("CFGD_COSIGN_BIN").is_err(),
                "drop must remove when no prior value existed"
            );
        }

        #[test]
        #[serial]
        fn argv_logging_enabled_records_invocations() {
            let shim = CosignTestShim::builder().with_argv_logging(true).install();
            let (code, _) = run_shim(&["sign", "--yes", "ghcr.io/test/x:v1"]);
            assert_eq!(code, 0);

            let log = shim.argv_log();
            assert!(log.contains("sign"), "argv log must contain `sign`: {log}");
            assert!(
                log.contains("--yes"),
                "argv log must contain `--yes`: {log}"
            );
            assert!(
                log.contains("ghcr.io/test/x:v1"),
                "argv log must contain artifact ref: {log}"
            );
            assert_eq!(shim.invocation_count(), 1);

            // Second invocation appends a new line.
            run_shim(&["verify", "ghcr.io/test/x:v1"]);
            assert_eq!(shim.invocation_count(), 2);
        }

        #[test]
        #[serial]
        fn argv_logging_disabled_does_not_write_log() {
            let shim = CosignTestShim::builder().with_argv_logging(false).install();
            assert!(
                std::env::var("CFGD_FAKE_COSIGN_LOG").is_err(),
                "argv-log env var must not be set when logging is disabled"
            );

            let (code, _) = run_shim(&["sign", "ghcr.io/test/x:v1"]);
            assert_eq!(code, 0);

            // No log file, no logged invocations.
            assert_eq!(shim.argv_log(), "");
            assert_eq!(shim.invocation_count(), 0);
        }

        #[test]
        #[serial]
        fn keygen_mode_writes_key_pair_to_cwd_on_generate_key_pair() {
            let _shim = CosignTestShim::builder().with_keygen(true).install();
            let workdir = tempfile::TempDir::new().expect("workdir");

            let bin = std::env::var("CFGD_COSIGN_BIN").unwrap();
            let status = std::process::Command::new(&bin)
                .arg("generate-key-pair")
                .current_dir(workdir.path())
                .status()
                .expect("spawn shim");
            assert!(status.success(), "keygen shim must exit zero");

            assert!(
                workdir.path().join("cosign.key").is_file(),
                "cosign.key must be written to cwd"
            );
            assert!(
                workdir.path().join("cosign.pub").is_file(),
                "cosign.pub must be written to cwd"
            );
            assert_eq!(
                std::fs::read(workdir.path().join("cosign.key")).unwrap(),
                b"fake-private-key-bytes"
            );
            assert_eq!(
                std::fs::read(workdir.path().join("cosign.pub")).unwrap(),
                b"fake-public-key-bytes"
            );
        }

        #[test]
        #[serial]
        fn keygen_mode_skips_writes_for_non_generate_subcommands() {
            let _shim = CosignTestShim::builder().with_keygen(true).install();
            let workdir = tempfile::TempDir::new().expect("workdir");

            let bin = std::env::var("CFGD_COSIGN_BIN").unwrap();
            let status = std::process::Command::new(&bin)
                .arg("sign")
                .arg("ghcr.io/test/x:v1")
                .current_dir(workdir.path())
                .status()
                .expect("spawn shim");
            assert!(status.success());

            assert!(
                !workdir.path().join("cosign.key").exists(),
                "non-keygen subcommand must NOT write cosign.key"
            );
            assert!(
                !workdir.path().join("cosign.pub").exists(),
                "non-keygen subcommand must NOT write cosign.pub"
            );
        }

        #[test]
        #[serial]
        fn exit_code_propagates_from_with_exit() {
            let _shim = CosignTestShim::builder().with_exit(1).install();
            let (code, _) = run_shim(&["sign", "ghcr.io/test/x:v1"]);
            assert_eq!(code, 1, "with_exit(1) must surface as non-zero exit");
        }

        #[test]
        #[serial]
        fn stderr_is_captured_from_with_stderr() {
            let _shim = CosignTestShim::builder()
                .with_exit(2)
                .with_stderr("oops something broke")
                .install();
            let (code, stderr) = run_shim(&["verify", "ghcr.io/test/x:v1"]);
            assert_eq!(code, 2);
            assert!(
                stderr.contains("oops something broke"),
                "shim stderr must surface: {stderr}"
            );
        }

        #[test]
        #[serial]
        fn stderr_round_trips_single_quotes() {
            let _shim = CosignTestShim::builder()
                .with_exit(1)
                .with_stderr("can't connect — 'rekor' down")
                .install();
            let (_code, stderr) = run_shim(&["sign"]);
            assert!(
                stderr.contains("can't connect — 'rekor' down"),
                "single-quote-laden stderr must round-trip: {stderr}"
            );
        }
    }
}
