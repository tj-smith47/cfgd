//! Reusable test mocks, builders and RAII pins for the process-global state a
//! test cannot otherwise reach.
//!
//! Compiled under `#[cfg(any(test, feature = "test-helpers"))]` (`lib.rs`),
//! so this module is part of every `cargo doc --all-features` render even
//! though only a consumer enabling `test-helpers` sees it outside a test build.
//!
//! # Which exclusion a TTL guard needs
//!
//! Every memo ceiling here is one process-global atomic and a test binary is
//! one process, so a concurrent pin from another test is visible. What that
//! costs is decided by what the test ASSERTS on, and the guards answer it three
//! different ways on purpose — copying a sibling's answer is how a test ends up
//! excluding the wrong thing. Ask what a concurrent pin could do to the claim:
//!
//! - the test asserts on an ANSWER the memo returns (`command_path` resolves
//!   this binary): a longer ceiling only lets entries live longer and a zero one
//!   only recomputes them, so neither changes the answer. No exclusion —
//!   `CommandPathMemoTtlGuard` carries none.
//! - the test asserts on a COUNT of the work the memo saved (one version query,
//!   one enumeration, one reused derivation): another test's zero pin makes the
//!   memoized answer recompute, which is exactly the number being measured. A
//!   named `serial_test` group, declared at every call site
//!   (`available_version_memo`, `enumeration_memo`, `tick_cache_reuse`; the
//!   availability sweep shares the UNNAMED group its own count tests use).
//! - the test asserts WHETHER SOMETHING HAPPENED at all (a transfer was
//!   attempted, or was spared), which is not a counter that can share a group:
//!   the lock inside the guard (`GitRefreshWindowGuard`), taken by construction
//!   so no call site can forget it.
//!
//! The lock is the strongest of the three and still the narrowest: it excludes
//! other PINS, never an unpinned concurrent reader of the same setting. Any
//! test whose claim turns on the pinned value must therefore pin — in whichever
//! direction it needs — rather than assume the default is live.

// Reusable test mocks and builders — gated behind `test-helpers` feature.
//
// Provides mock implementations of the core provider traits (FileManager,
// SecretBackend, SecretProvider, SystemConfigurator) plus a TestEnvBuilder
// for creating isolated temp directories with config/profile/module layouts.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};

use secrecy::SecretString;

use crate::errors::{CfgdError, FileError, SecretError};
use crate::output::Printer;
use crate::providers::{
    FileAction, FileDiff, FileDriftResult, FileLayer, FileTree, SecretBackend, SecretProvider,
    SystemConfigurator, SystemDrift,
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
    pub content_drift_calls: Mutex<Vec<String>>,
    pub fail_apply: Mutex<bool>,
    /// When set, `content_drift` returns this result verbatim instead of deriving
    /// the outcome from on-disk content. Lets tests pin an exact drift shape.
    pub content_drift_result: Mutex<Option<FileDriftResult>>,
    /// What `link_deployed_content_hashes` reports. Empty by default — the mock
    /// deploys nothing — so only a test that has recorded a managed row for the
    /// same target fills it.
    pub link_deployed: Mutex<Vec<crate::providers::LinkDeployedRow>>,
}

impl MockFileManager {
    pub fn new() -> Self {
        Self {
            scan_source_calls: Mutex::new(Vec::new()),
            scan_target_calls: Mutex::new(Vec::new()),
            diff_calls: Mutex::new(Vec::new()),
            apply_calls: Mutex::new(Vec::new()),
            content_drift_calls: Mutex::new(Vec::new()),
            fail_apply: Mutex::new(false),
            content_drift_result: Mutex::new(None),
            link_deployed: Mutex::new(Vec::new()),
        }
    }

    /// Pin what `link_deployed_content_hashes` reports, so a caller can drive
    /// the recorded-hash refresh without a real symlink deployment.
    pub fn set_link_deployed(&self, rows: Vec<crate::providers::LinkDeployedRow>) {
        *self.link_deployed.lock().unwrap() = rows;
    }

    pub fn set_fail_apply(&self, fail: bool) {
        *self.fail_apply.lock().unwrap() = fail;
    }

    /// Pin the [`FileDriftResult`] that `content_drift` returns regardless of
    /// the source/target arguments.
    pub fn set_content_drift_result(&self, result: FileDriftResult) {
        *self.content_drift_result.lock().unwrap() = Some(result);
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

    fn content_drift(
        &self,
        source: &Path,
        target: &Path,
        _origin: Option<&str>,
        _strategy: Option<crate::config::FileStrategy>,
    ) -> crate::errors::Result<FileDriftResult> {
        self.content_drift_calls
            .lock()
            .unwrap()
            .push(target.display().to_string());

        if let Some(pinned) = self.content_drift_result.lock().unwrap().clone() {
            return Ok(pinned);
        }

        // Mirror production (`CfgdFileManager::file_drift_one`): tilde-expand the
        // target and report it POSIX-normalized so tests see the same shape.
        let target_path = crate::expand_tilde(target);
        let target_id = crate::PathDisplayExt::display_posix(&target_path);
        if !source.exists() {
            return Ok(FileDriftResult {
                target: target_id,
                matches: false,
                expected: "managed source present".to_string(),
                actual: "source not found".to_string(),
                unmanaged: false,
            });
        }
        if !target_path.exists() {
            return Ok(FileDriftResult {
                target: target_id,
                matches: false,
                expected: "present".to_string(),
                actual: "missing".to_string(),
                unmanaged: false,
            });
        }
        // Only a successful read on BOTH sides counts as a comparison; if either
        // read fails, report drift rather than letting two `None`s compare equal.
        let matches = matches!(
            (std::fs::read(source), std::fs::read(&target_path)),
            (Ok(a), Ok(b)) if a == b
        );
        Ok(FileDriftResult {
            target: target_id,
            matches,
            expected: "content matches source".to_string(),
            actual: if matches {
                "content matches source".to_string()
            } else {
                "content differs from source".to_string()
            },
            unmanaged: false,
        })
    }

    /// The mock deploys nothing, so it reports no link-deployed content unless
    /// a test pinned some through [`MockFileManager::set_link_deployed`].
    fn link_deployed_content_hashes(
        &self,
        _profile: &crate::config::MergedProfile,
    ) -> crate::errors::Result<Vec<crate::providers::LinkDeployedRow>> {
        Ok(self.link_deployed.lock().unwrap().clone())
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

    fn apply(
        &self,
        desired: &serde_yaml::Value,
        _cx: &crate::providers::SystemContext<'_>,
    ) -> crate::errors::Result<()> {
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

/// Build a `file://` URL portable across unix and windows.
///
/// Thin alias for [`crate::to_file_url`] — kept under `test_helpers` so existing
/// test callers compile unchanged.
pub fn file_url(path: &Path) -> String {
    crate::to_file_url(path)
}

/// A fixture root as code that FOLDS symlinks will judge it.
///
/// `std::env::temp_dir()` is itself reached through a symlink on macOS, where
/// `/var` is a link to `private/var` and `$TMPDIR` lives under it, so the first
/// link any ancestor walk meets under `tempfile::tempdir()` is that one and every
/// path it goes on to compose carries a `/private` prefix. A fixture building its
/// expectation out of the tempdir's own path is then comparing against a prefix
/// the code under test cannot produce. Rebase the whole fixture on this root once
/// and build every directory, link and expectation from it.
///
/// For the root only. A path that IS a symlink must not be passed: resolving it
/// answers where it points, which is not what a fold of its own ancestors
/// composes. A test asserting a path as the operator GAVE it keeps comparing
/// against the unfolded path, that being the string its subject renders.
pub fn folded_temp_root(root: &Path) -> PathBuf {
    std::fs::canonicalize(root).unwrap_or_else(|e| {
        panic!(
            "{}: a fixture root must exist before it can be folded: {e}",
            root.display()
        )
    })
}

/// Shared snapshot-golden assertion for output snapshot tests.
///
/// `base.join(name)` is the golden file. With `INSTA_UPDATE=always` (or when
/// the golden doesn't yet exist), `actual` is written to disk and the function
/// returns without asserting — supports the standard insta-style regen flow.
/// Otherwise: both sides are CRLF→LF normalized and compared with
/// `pretty_assertions::assert_eq!`, which produces an inline diff on
/// mismatch.
///
/// This replaces 41 per-file `fn assert_snapshot` definitions whose bodies
/// drifted independently. Callers route any tempdir-rooted text through
/// [`crate::normalize_for_snapshot`] BEFORE handing it to this function;
/// keeping the CRLF fold here guards against the harness regressing back to
/// host-dependent line endings.
///
/// `cfgd_version` is the version literal folded to `<VERSION>` on both sides
/// — the *consuming test crate's* `env!("CARGO_PKG_VERSION")`, never
/// cfgd-core's own (crates version independently). Invoke through the
/// [`crate::assert_snapshot_golden!`] macro, which captures the invoking
/// crate's version automatically.
pub fn assert_snapshot_golden(base: &Path, name: &str, actual: &str, cfgd_version: &str) {
    let path = base.join(name);
    if std::env::var("INSTA_UPDATE").as_deref() == Ok("always") || !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create snapshot parent dir");
        }
        // Regenerate with the version folded to `<VERSION>` so regenerated
        // goldens stay version-agnostic rather than re-pinning the literal.
        let regen = crate::normalize_cfgd_version(actual, cfgd_version);
        std::fs::write(&path, regen.as_ref()).expect("write snapshot golden");
        return;
    }
    let expected = std::fs::read_to_string(&path).expect("read snapshot golden");
    // Fold the running cfgd version to `<VERSION>` on both sides so
    // version-bearing goldens survive release bumps. Applied to `expected`
    // too in case a golden was regenerated with a literal version rather than
    // the placeholder.
    let actual_lf = crate::normalize_line_endings(actual);
    let expected_lf = crate::normalize_line_endings(&expected);
    let actual_norm = crate::normalize_cfgd_version(&actual_lf, cfgd_version);
    let expected_norm = crate::normalize_cfgd_version(&expected_lf, cfgd_version);
    pretty_assertions::assert_eq!(actual_norm, expected_norm, "snapshot mismatch: {name}");
}

/// Snapshot-golden assertion that folds the *invoking crate's* running
/// version to `<VERSION>`.
///
/// A macro rather than a function so `env!("CARGO_PKG_VERSION")` expands in
/// the test crate that owns the captured output (the cfgd binary crate for
/// CLI snapshots) — cfgd-core's own version is wrong the moment the crates'
/// release cadences diverge.
#[macro_export]
macro_rules! assert_snapshot_golden {
    ($base:expr, $name:expr, $actual:expr $(,)?) => {
        $crate::test_helpers::assert_snapshot_golden(
            $base,
            $name,
            $actual,
            env!("CARGO_PKG_VERSION"),
        )
    };
}

/// Fail when two of the slots a fixture asserts hold one value.
///
/// A fixture whose asserted counts are meant to tell its slots APART proves
/// nothing about which slot holds which the moment two of them coincide: a
/// producer that swapped `succeeded` and `skipped`, or wired a class count to
/// its sibling's, renders exactly the numbers the assertion expects. Carrying
/// that premise as a sentence in a doc comment is what let four fixtures claim
/// a distinctness their own numbers refuted, so the premise runs here instead
/// and the doc comment points at this call.
///
/// Pass every slot whose value a producer could read for another, with the name
/// a reader of the failure would recognize. A ZERO slot is left out: its clause
/// either renders nothing or renders `0`, so two of them are no coincidence,
/// and only a value a wrong wiring could return in another slot's place is a
/// hole.
pub fn assert_slots_discriminate(slots: &[(&str, usize)]) {
    assert!(
        slots.len() >= 2,
        "a distinctness premise needs two slots to tell apart, got {slots:?}"
    );
    for (index, (name, value)) in slots.iter().enumerate() {
        for (other, other_value) in &slots[index + 1..] {
            assert_ne!(
                value, other_value,
                "`{name}` and `{other}` both hold {value}, so a producer reading \
                 one for the other renders the number this fixture expects: {slots:?}"
            );
        }
    }
}

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
// BareGitRepo — bare-repo fixture for tests needing a "remote"
// ---------------------------------------------------------------------------

/// A commit specification for the `BareGitRepoBuilder`.
struct BareGitCommitSpec {
    message: String,
    files: Vec<(String, String)>,
}

/// A branch specification: branch name plus commits to add on top of the main
/// branch tip when the branch was created.
struct BareGitBranchSpec {
    name: String,
    files: Vec<(String, String)>,
}

/// Builder for a bare git repository used as a test "remote".
///
/// Creates a bare repo backed by `tempfile::TempDir`, populated via a temporary
/// working clone. Supports adding sequential commits, tags (on HEAD at the time
/// `.tag()` is called), and branches with additional files.
pub struct BareGitRepoBuilder {
    commits: Vec<BareGitCommitSpec>,
    tags: Vec<(String, usize)>,
    branches: Vec<BareGitBranchSpec>,
}

impl BareGitRepoBuilder {
    fn new() -> Self {
        Self {
            commits: Vec::new(),
            tags: Vec::new(),
            branches: Vec::new(),
        }
    }

    /// Add a commit with the given message and file contents.
    /// Files are specified as `(path, content)` pairs.
    pub fn commit(mut self, message: &str, files: &[(&str, &str)]) -> Self {
        self.commits.push(BareGitCommitSpec {
            message: message.to_string(),
            files: files
                .iter()
                .map(|(p, c)| (p.to_string(), c.to_string()))
                .collect(),
        });
        self
    }

    /// Tag the most recent commit (at the time of this call in builder order).
    /// Panics if no commits have been added yet.
    pub fn tag(mut self, name: &str) -> Self {
        assert!(
            !self.commits.is_empty(),
            "BareGitRepoBuilder::tag() requires at least one prior commit"
        );
        self.tags.push((name.to_string(), self.commits.len() - 1));
        self
    }

    /// Create a branch off the current HEAD with an additional commit
    /// containing the given files.
    pub fn branch(mut self, name: &str, files: &[(&str, &str)]) -> Self {
        self.branches.push(BareGitBranchSpec {
            name: name.to_string(),
            files: files
                .iter()
                .map(|(p, c)| (p.to_string(), c.to_string()))
                .collect(),
        });
        self
    }

    /// Build the bare repository and return a `BareGitRepo` handle.
    pub fn build(self) -> BareGitRepo {
        assert!(
            !self.commits.is_empty(),
            "BareGitRepoBuilder requires at least one commit"
        );

        let bare_dir = tempfile::TempDir::new().expect("create bare repo tempdir");
        let work_dir = tempfile::TempDir::new().expect("create working clone tempdir");

        let bare_repo = git2::Repository::init_bare(bare_dir.path()).expect("git init --bare");

        // Working clone to make commits
        let work_path = work_dir.path().join("work");
        let work_repo = git2::Repository::init(&work_path).expect("git init work clone");

        // Configure committer identity
        let mut config = work_repo.config().expect("repo config");
        config
            .set_str("user.name", "cfgd-test")
            .expect("set user.name");
        config
            .set_str("user.email", "test@cfgd.io")
            .expect("set user.email");

        // Add bare as remote
        let bare_url = file_url(bare_dir.path());
        work_repo
            .remote("origin", &bare_url)
            .expect("add origin remote");

        let sig = git2::Signature::now("cfgd-test", "test@cfgd.io").expect("signature");

        // Apply commits sequentially, tracking OIDs for tag placement
        let mut commit_oids: Vec<git2::Oid> = Vec::new();
        for spec in &self.commits {
            for (path, content) in &spec.files {
                let full_path = work_path.join(path);
                if let Some(parent) = full_path.parent() {
                    std::fs::create_dir_all(parent).expect("create parent dirs");
                }
                std::fs::write(&full_path, content).expect("write file");
            }

            let mut index = work_repo.index().expect("repo index");
            index
                .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
                .expect("add all to index");
            index.write().expect("write index");

            let tree_id = index.write_tree().expect("write tree");
            let tree = work_repo.find_tree(tree_id).expect("find tree");

            let parents: Vec<git2::Commit<'_>> = if commit_oids.is_empty() {
                vec![]
            } else {
                let last_oid = *commit_oids.last().expect("last oid");
                vec![work_repo.find_commit(last_oid).expect("find parent commit")]
            };
            let parent_refs: Vec<&git2::Commit<'_>> = parents.iter().collect();

            let oid = work_repo
                .commit(Some("HEAD"), &sig, &sig, &spec.message, &tree, &parent_refs)
                .expect("commit");
            commit_oids.push(oid);
        }

        // Determine the branch name from HEAD
        let head_branch = work_repo
            .head()
            .expect("HEAD")
            .shorthand()
            .unwrap_or("master")
            .to_string();

        // Push main branch to bare
        let mut remote = work_repo.find_remote("origin").expect("find origin remote");
        remote
            .push(
                &[&format!(
                    "refs/heads/{head_branch}:refs/heads/{head_branch}"
                )],
                None,
            )
            .expect("push main branch to bare");

        // Create tags on the bare repo
        for (tag_name, commit_idx) in &self.tags {
            let oid = commit_oids[*commit_idx];
            let obj = bare_repo
                .find_object(oid, None)
                .expect("find tagged object in bare");
            bare_repo
                .tag_lightweight(tag_name, &obj, false)
                .expect("create tag in bare");
        }

        // Create branches
        for branch_spec in &self.branches {
            // Start from HEAD of the working clone
            let head_commit = work_repo
                .head()
                .expect("HEAD")
                .peel_to_commit()
                .expect("peel HEAD to commit");

            // Create branch at HEAD
            work_repo
                .branch(&branch_spec.name, &head_commit, false)
                .expect("create branch");

            // Checkout the branch
            work_repo
                .set_head(&format!("refs/heads/{}", branch_spec.name))
                .expect("set HEAD to branch");
            work_repo
                .checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
                .expect("checkout branch");

            // Add files and commit on the branch
            for (path, content) in &branch_spec.files {
                let full_path = work_path.join(path);
                if let Some(parent) = full_path.parent() {
                    std::fs::create_dir_all(parent).expect("create parent dirs");
                }
                std::fs::write(&full_path, content).expect("write file");
            }

            let mut index = work_repo.index().expect("repo index");
            index
                .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
                .expect("add all to index");
            index.write().expect("write index");

            let tree_id = index.write_tree().expect("write tree");
            let tree = work_repo.find_tree(tree_id).expect("find tree");
            let branch_head = work_repo
                .head()
                .expect("HEAD")
                .peel_to_commit()
                .expect("peel HEAD");

            work_repo
                .commit(
                    Some("HEAD"),
                    &sig,
                    &sig,
                    &format!("branch commit: {}", branch_spec.name),
                    &tree,
                    &[&branch_head],
                )
                .expect("commit on branch");

            // Push branch to bare
            remote
                .push(
                    &[&format!(
                        "refs/heads/{}:refs/heads/{}",
                        branch_spec.name, branch_spec.name
                    )],
                    None,
                )
                .expect("push branch to bare");

            // Return to main branch
            work_repo
                .set_head(&format!("refs/heads/{head_branch}"))
                .expect("set HEAD back to main");
            work_repo
                .checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
                .expect("checkout main");
        }

        BareGitRepo {
            bare_path: bare_repo.path().to_path_buf(),
            _bare_dir: bare_dir,
            _work_dir: work_dir,
            bare_repo: Some(bare_repo),
            head_branch,
        }
    }
}

/// A bare git repository fixture for tests that need a "remote" to clone/fetch.
///
/// Created via `BareGitRepo::builder()`. The bare repo is backed by a
/// `TempDir` and cleaned up automatically on drop.
pub struct BareGitRepo {
    _bare_dir: tempfile::TempDir,
    _work_dir: tempfile::TempDir,
    bare_path: std::path::PathBuf,
    bare_repo: Option<git2::Repository>,
    head_branch: String,
}

impl BareGitRepo {
    /// Start building a bare git repo fixture.
    pub fn builder() -> BareGitRepoBuilder {
        BareGitRepoBuilder::new()
    }

    /// The `file://` URL for this bare repo, suitable for clone/fetch.
    pub fn url(&self) -> String {
        file_url(&self.bare_path)
    }

    /// The path to the bare repo on disk.
    pub fn path(&self) -> &Path {
        &self.bare_path
    }

    /// Take the upstream out of service, so any later transfer against it
    /// fails and a cache-served read is provably cache-served.
    ///
    /// `remove_dir_all` alone is not portable: this fixture holds the
    /// repository open, libgit2 keeps mapped packfiles in a process-global
    /// cache, and Windows refuses to unlink an open or mapped file. The held
    /// handle is dropped first; if the removal is still refused, the
    /// repository is broken in place (HEAD, config, refs) until it stops
    /// answering. Query methods (`has_tag`, `tags`, …) panic after this.
    // env-mutator-ok: deletes a fixture repo; its `drop(…)` is `std::mem::drop`, not a guard's.
    pub fn remove_upstream(&mut self) {
        drop(self.bare_repo.take());
        if std::fs::remove_dir_all(&self.bare_path).is_ok() {
            return;
        }
        for f in ["HEAD", "config", "packed-refs"] {
            let _ = std::fs::remove_file(self.bare_path.join(f));
        }
        for d in ["refs", "info"] {
            let _ = std::fs::remove_dir_all(self.bare_path.join(d));
        }
    }

    fn repo(&self) -> &git2::Repository {
        self.bare_repo
            .as_ref()
            .expect("the upstream was removed by remove_upstream")
    }

    /// The name of the main branch (usually "master" or "main").
    pub fn head_branch(&self) -> &str {
        &self.head_branch
    }

    /// Commit onto the default branch AFTER the fixture was built, the way an
    /// upstream moves between two of a test's own fetches.
    ///
    /// The builder's working clone is gone by then, so the commit is written
    /// straight into the bare repository: each entry replaces or adds one
    /// top-level file over the current tip's tree. Paths are single-segment —
    /// a nested path needs a tree per level and no fixture has wanted one.
    pub fn publish_commit(&self, message: &str, files: &[(&str, &str)]) -> git2::Oid {
        let repo = git2::Repository::open_bare(self.path()).expect("open bare repo");
        let branch_ref = format!("refs/heads/{}", self.head_branch);
        let parent_oid = repo
            .refname_to_id(&branch_ref)
            .expect("resolve head branch");
        let parent = repo.find_commit(parent_oid).expect("find tip commit");
        let parent_tree = parent.tree().expect("tip tree");

        let mut builder = repo
            .treebuilder(Some(&parent_tree))
            .expect("tree builder over the tip");
        for (path, content) in files {
            assert!(
                !path.contains('/'),
                "publish_commit takes top-level paths only, got {path}"
            );
            let blob = repo.blob(content.as_bytes()).expect("write blob");
            builder.insert(path, blob, 0o100644).expect("insert blob");
        }
        let tree_id = builder.write().expect("write tree");
        let tree = repo.find_tree(tree_id).expect("find written tree");

        let sig = git2::Signature::now("cfgd-test", "test@cfgd.io").expect("signature");
        repo.commit(Some(&branch_ref), &sig, &sig, message, &tree, &[&parent])
            .expect("commit onto the default branch")
    }

    /// Check whether a lightweight tag exists in the bare repo.
    pub fn has_tag(&self, name: &str) -> bool {
        self.repo()
            .find_reference(&format!("refs/tags/{name}"))
            .is_ok()
    }

    /// Check whether a branch exists in the bare repo.
    pub fn has_branch(&self, name: &str) -> bool {
        self.repo()
            .find_reference(&format!("refs/heads/{name}"))
            .is_ok()
    }

    /// List all tag names in the bare repo.
    pub fn tags(&self) -> Vec<String> {
        self.repo()
            .tag_names(None)
            .map(|names| {
                names
                    .iter()
                    .filter_map(|n| n.ok().flatten())
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// Printer helper
// ---------------------------------------------------------------------------

/// The glyphs a SETTLED status line can start with. A running window's `◐` is
/// not one: it is repainted in place and is never the action's own line.
///
/// ONE definition on purpose. Two of them is how a side-channel `◉` came to
/// sit beside a tree line for the same action while the fence guarding that
/// action still read as passing.
pub const SETTLED_GLYPHS: [char; 5] = ['\u{2713}', '\u{2717}', '\u{26A0}', '\u{2205}', '\u{25C9}'];

/// The settled status lines of a captured transcript, trimmed and in order.
/// Strip ANSI before calling: a styled glyph is preceded by its escape.
pub fn settled_status_lines(transcript: &str) -> Vec<String> {
    transcript
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with(SETTLED_GLYPHS))
        .map(str::to_string)
        .collect()
}

/// Read a `Printer::for_test*` capture buffer with ANSI escapes removed — the
/// ONE way a test should reach the string it asserts against.
///
/// Colour is no longer ambient: every capture constructor pins `colors: false`
/// at construction, so a buffer is unstyled BY CONSTRUCTION rather than because
/// this function strips it. Keep using it anyway for any assertion about TEXT —
/// `Printer::for_test_with_theme_colored` really does emit escapes, an
/// attribute-carrying slot emits SGR even with colour off (`NO_COLOR` governs
/// colour only), and a subject may carry foreign escapes of its own.
///
/// Three failure shapes, all observed on the raw buffer while colour was still
/// ambient: `contains("module:vim-config")` breaks on the escape between the
/// owner token's two styled halves, `ends_with(path)` breaks on the trailing
/// reset, and every negative `!contains(…)` passes vacuously once styling is
/// on — it stops guarding anything without ever going red.
///
/// A test that asserts ON the escapes wants the raw buffer and
/// `Printer::for_test_with_theme_colored`. To assert the colour DECISION rather
/// than a rendered escape, call `output::printer::colors_must_be_disabled` and
/// render nothing.
pub fn captured_text(buf: &std::sync::Arc<std::sync::Mutex<String>>) -> String {
    crate::output::strip_ansi(&buf.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Create a quiet `Printer` for tests that exercise the reconciler entry
/// surface (`Reconciler::apply`, `Reconciler::apply_action`, and per-action
/// helpers in `apply.rs` / `modules.rs` / `packages.rs` / `secrets.rs` /
/// `system.rs`) plus mock trait impls (`MockPackageManager`,
/// `MockSecretBackend`, `MockSystemConfigurator`).
///
/// Returns a bare `Printer` (not the `(Printer, Buffer)` tuple from
/// `Printer::for_test()`) so it drops in as a direct replacement in fixtures
/// that don't assert on captured output.
///
/// Built from the capture constructor and not from `Printer::new`, because
/// `new` inherits the terminal the suite was invoked from: under a pty that
/// printer reports a live region AND a human at stdin, so a command reaching
/// an unanswered confirmation prompt BLOCKS for the rest of the run instead of
/// refusing. Discarding the buffer keeps the surface identical (Quiet, Table).
pub fn test_printer() -> crate::output::Printer {
    crate::output::Printer::for_test().0
}

/// A Rust source's logical lines: every `\`-continued string literal folded
/// onto the line that opened it, paired with that opening line's 1-based
/// number.
///
/// A source-walking pin looks for two tells on ONE line, and a literal
/// continued across two physical lines carries them on two — so a walk reading
/// `body.lines()` directly is evaded by a line break. Two shapes make a naive
/// fold wrong, and both are handled here: a literal ENDING in an escaped
/// backslash (`"foo\\"`) is not continued, judged by an odd trailing count;
/// and a RAW literal has no escapes at all, so neither the line opening one
/// nor any line inside it can be continued.
///
/// The raw-literal scan ignores ordinary string literals, so an `r#` written
/// inside one is read as an opener. That direction is the safe one: it can
/// only SUPPRESS a fold — costing a walk one offender it would have caught —
/// never join two lines that were never one.
pub fn logical_source_lines(body: &str) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    let mut continues = false;
    let mut raw_hashes: Option<usize> = None;
    for (n, line) in body.lines().enumerate() {
        let opened_inside_raw = raw_hashes.is_some();
        scan_raw_literals(line, &mut raw_hashes);
        let trimmed = line.trim_end();
        let trailing = trimmed.chars().rev().take_while(|c| *c == '\\').count();
        let opens_next =
            !opened_inside_raw && raw_hashes.is_none() && trailing % 2 == 1 && !trimmed.is_empty();
        let piece = if opens_next {
            &trimmed[..trimmed.len() - 1]
        } else {
            trimmed
        };
        match out.last_mut() {
            Some((_, acc)) if continues => acc.push_str(piece.trim_start()),
            _ => out.push((n + 1, piece.to_string())),
        }
        continues = opens_next;
    }
    out
}

/// A raw literal opening at `bytes[i]` (`r`, some `#`s, a `"`): its hash
/// count, or `None` when nothing opens there.
///
/// `pub(crate)` because two scanners read this arithmetic — the raw-only scan
/// below and the fixture walk's string-aware line scan — and an off-by-one
/// carried by only one of them would let the two disagree about where a
/// literal ends.
pub(crate) fn raw_string_open(bytes: &[u8], i: usize) -> Option<usize> {
    if bytes[i] != b'r' {
        return None;
    }
    let open = bytes[i + 1..].iter().take_while(|b| **b == b'#').count();
    (bytes.get(i + 1 + open) == Some(&b'"')).then_some(open)
}

/// Whether the raw literal opened with `open` hashes closes at `bytes[i]` (a
/// `"` followed by at least as many `#`s). The twin of [`raw_string_open`],
/// shared for the same reason.
pub(crate) fn raw_string_closes(bytes: &[u8], i: usize, open: usize) -> bool {
    bytes[i] == b'"' && bytes[i + 1..].iter().take_while(|b| **b == b'#').count() >= open
}

/// Advance the raw-literal state across one physical line: `r`, some `#`s and
/// a `"` opens one; a `"` followed by the same number of `#`s closes it.
fn scan_raw_literals(line: &str, hashes: &mut Option<usize>) {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match *hashes {
            Some(open) => {
                if raw_string_closes(bytes, i, open) {
                    *hashes = None;
                    i += 1 + open;
                    continue;
                }
            }
            None => {
                if let Some(open) = raw_string_open(bytes, i) {
                    *hashes = Some(open);
                    i += 2 + open;
                    continue;
                }
            }
        }
        i += 1;
    }
}

/// A `PackageStateStore` that remembers nothing — for a fixture whose subject
/// (`bootstrap`) reaches no state. A test-fixture stub only; re-exported under
/// this name so existing fixtures keep reading as "the state a bootstrap-only
/// test doesn't need."
pub use crate::providers::NoOpPackageState as NullPackageState;

/// A `PackageContext` for a fixture that drives `bootstrap`, which touches no
/// state — so the fixture needs no `StateStore` of its own.
pub fn test_bootstrap_context(
    printer: &crate::output::Printer,
) -> crate::providers::PackageContext<'_> {
    crate::providers::PackageContext::new(printer, &NullPackageState)
}

/// [`test_bootstrap_context`] over a caller-owned sink, for a fixture asserting
/// that a bootstrap's post-install caveats travel back to the reconciler.
pub fn test_bootstrap_context_with_notes<'a>(
    printer: &'a crate::output::Printer,
    notes: &'a crate::providers::NoteSink,
) -> crate::providers::PackageContext<'a> {
    crate::providers::PackageContext::with_notes(printer, &NullPackageState, notes)
}

/// Build a `PackageContext` from a borrowed `Printer` and `StateStore` — the
/// pair every `PackageManager` fixture now needs alongside `test_printer()` /
/// `test_state()` since `PackageContext` threading replaced the bare
/// `&Printer` parameter on the state-touching trait methods.
pub fn test_package_context<'a>(
    printer: &'a crate::output::Printer,
    state: &'a crate::state::StateStore,
) -> crate::providers::PackageContext<'a> {
    crate::providers::PackageContext::new(printer, state)
}

// ---------------------------------------------------------------------------
// NoopDaemonHooks
// ---------------------------------------------------------------------------

/// Empty `DaemonHooks` implementation that returns an empty `ProviderRegistry`
/// and zero file/package actions. Use in daemon tests that exercise pure
/// scheduling/state-machine logic and don't care about plan output.
pub struct NoopDaemonHooks;

impl crate::daemon::DaemonHooks for NoopDaemonHooks {
    fn build_registry(&self, _: &crate::config::CfgdConfig) -> crate::providers::ProviderRegistry {
        crate::providers::ProviderRegistry::new()
    }

    fn plan_files(
        &self,
        _: &std::path::Path,
        _: &crate::config::ResolvedProfile,
    ) -> crate::errors::Result<Vec<crate::providers::FileAction>> {
        Ok(vec![])
    }

    fn plan_packages(
        &self,
        _: &crate::config::MergedProfile,
        _: &[&dyn crate::providers::PackageManager],
        _: &std::collections::HashSet<String>,
        _: &crate::providers::PackageContext<'_>,
    ) -> crate::errors::Result<Vec<crate::providers::PackageAction>> {
        Ok(vec![])
    }

    fn extend_registry_custom_managers(
        &self,
        _: &mut crate::providers::ProviderRegistry,
        _: &crate::config::PackagesSpec,
    ) {
    }

    fn expand_tilde(&self, path: &std::path::Path) -> PathBuf {
        crate::expand_tilde(path)
    }
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
        dep_pulled: false,
        name: name.to_string(),
        packages: vec![
            crate::modules::ResolvedPackage {
                canonical_name: "neovim".to_string(),
                resolved_name: "neovim".to_string(),
                manager: "brew".to_string(),
                manager_declared: false,
                version: Some("0.10.2".to_string()),
                script: None,
                creates: None,
                only_if: None,
                unless: None,
                min_version: None,
            },
            crate::modules::ResolvedPackage {
                canonical_name: "ripgrep".to_string(),
                resolved_name: "ripgrep".to_string(),
                manager: "brew".to_string(),
                manager_declared: false,
                version: Some("14.1.0".to_string()),
                script: None,
                creates: None,
                only_if: None,
                unless: None,
                min_version: None,
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
        on_drift_scripts: Vec::new(),
        system: std::collections::BTreeMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
        platform_skip_reason: None,
        origin: None,
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
                version: None,
                name: name.to_string(),
                spec: crate::config::ModuleSpec {
                    depends: deps.iter().map(|s| s.to_string()).collect(),
                    ..Default::default()
                },
                dir: PathBuf::from(format!("/fake/{name}")),
                origin: None,
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

/// A `PATH` holding exactly the named executables and nothing else, for a test
/// asserting what a `command_available` probe resolves.
///
/// It exists because an `is_available()` test that compares the provider's
/// answer to `command_available("<tool>")` restates the implementation: both
/// sides move together, so the test passes on every host and would keep passing
/// if the probed name were misspelled. Naming the executables makes the probe's
/// NAME the subject — install `gsettings` and the configurator must say yes;
/// install nothing and it must say no.
///
/// Take `path_env_mutation_guard()` first (declared before this, so it drops
/// last) and pair the test with `serial_test::serial`: `PATH` is process-global.
/// `BootstrappedPathDirsGuard::capture_and_clear()` is required too for the
/// negative direction — the bootstrapped registry is searched after `PATH`.
///
/// Unix-only: the probe files are `/bin/sh` no-ops, and Windows resolves an
/// executable by `PATHEXT` rather than by the exec bit.
#[cfg(unix)]
pub struct ProbePath {
    _tmp: tempfile::TempDir,
    _path: EnvVarGuard,
}

#[cfg(unix)]
impl ProbePath {
    /// A `PATH` of one directory containing an executable per name.
    pub fn containing(names: &[&str]) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().expect("tempdir");
        for name in names {
            let bin = tmp.path().join(name);
            std::fs::write(&bin, "#!/bin/sh\nexit 0\n").expect("write probe tool");
            let mut perms = std::fs::metadata(&bin).expect("stat").permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&bin, perms).expect("chmod");
        }
        let path = EnvVarGuard::set("PATH", tmp.path().to_str().expect("utf-8 tempdir"));
        Self {
            _tmp: tmp,
            _path: path,
        }
    }
}

/// One argv-dispatch arm of a shim written by [`write_tool_shim`].
///
/// `matches` is a substring of the invocation's space-joined argv; the FIRST
/// arm whose substring is present answers the call. An empty `matches` always
/// answers, so it is how a shim spells its fallback behaviour — an arm after
/// one is unreachable.
pub struct ShimArm<'a> {
    /// The argv substring this arm answers; `""` answers every invocation.
    pub matches: &'a str,
    /// Written verbatim to stdout — no trailing newline is added.
    pub stdout: &'a str,
    /// Written verbatim to stderr — no trailing newline is added.
    pub stderr: &'a str,
    /// The status the shim exits with once this arm has answered.
    pub exit_code: i32,
}

impl<'a> ShimArm<'a> {
    /// An arm answering every invocation naming `matches` with `stdout` and a
    /// zero exit — a stand-in that fakes one subcommand's listing.
    pub fn on(matches: &'a str, stdout: &'a str) -> Self {
        Self {
            matches,
            stdout,
            stderr: "",
            exit_code: 0,
        }
    }

    /// The fallback arm: answers every invocation no earlier arm claimed.
    pub fn always(stdout: &'a str, stderr: &'a str, exit_code: i32) -> Self {
        Self {
            matches: "",
            stdout,
            stderr,
            exit_code,
        }
    }
}

/// Write an executable stand-in for an external tool into `dir` and return the
/// path to invoke it by. Every invocation appends its space-joined argv as one
/// line to `<dir>/argv.log`, then the first matching [`ShimArm`] decides what
/// the shim writes and what it exits with.
///
/// Two arms, one per host family, with identical observable behaviour:
///
/// - **Unix** — a `/bin/sh` script, `chmod 0755`, named `name`. Canned output
///   is emitted through `printf '%s'`, single-quote-escaped into the script.
/// - **Windows** — a `@echo off` batch file named `<name>.cmd`, which
///   `std::process::Command` executes through `cmd.exe` when it is named by an
///   absolute path (as a `CFGD_*_BIN` seam names it). Canned output lives in
///   sidecar files beside the script and is emitted with `type`, because
///   `echo` would append CRLF and demand `^`-escaping of everything a package
///   listing contains; argv dispatch is `findstr /C:` over the command line.
///
/// Three Windows-only limits a caller must respect:
///
/// - **A tool cfgd invokes with a `\r` or `\n` in an argument cannot be
///   shimmed here at all.** Those two bytes truncate a `cmd.exe` command
///   line, so `std::process::Command` refuses them outright when the program
///   is a batch file (`ErrorKind::InvalidInput`, "batch file arguments are
///   invalid"). A `%` is fine: std neutralizes it with the `%%cd:~,%` no-op
///   substring trick rather than rejecting it. The cases in this workspace
///   are the two whole-listing argvs — `rpm --queryformat
///   "%{NAME}\t%{VERSION}\n"` and `dpkg-query -f=${Package}\t${Version}\n` —
///   whose trailing newline is the record separator; a test standing in for
///   one stays `#[cfg(unix)]` and says so.
/// - A `matches` substring may not contain `"` — `findstr /C:` takes it as
///   the pattern's own terminator.
/// - An argv carrying `&`, `|`, `<` or `>` corrupts the log line.
pub fn write_tool_shim(dir: &Path, name: &str, arms: &[ShimArm<'_>]) -> std::path::PathBuf {
    let log_path = dir.join("argv.log");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        // Single-quote-safe escaping: replace ' with '\''.
        let sq = |s: &str| s.replace('\'', "'\\''");
        let mut script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n",
            sq(&log_path.display().to_string())
        );
        let mut answered = false;
        for arm in arms {
            let (out, err, code) = (sq(arm.stdout), sq(arm.stderr), arm.exit_code);
            if arm.matches.is_empty() {
                script.push_str(&format!(
                    "printf '%s' '{out}'\nprintf '%s' '{err}' 1>&2\nexit {code}\n"
                ));
                answered = true;
                break;
            }
            script.push_str(&format!(
                "case \"$*\" in\n*'{}'*) printf '%s' '{out}'; printf '%s' '{err}' 1>&2; exit {code} ;;\nesac\n",
                sq(arm.matches)
            ));
        }
        if !answered {
            script.push_str("exit 0\n");
        }

        let bin_path = dir.join(name);
        std::fs::write(&bin_path, script).expect("write shim");
        let mut perms = std::fs::metadata(&bin_path).expect("stat").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin_path, perms).expect("chmod");
        bin_path
    }

    #[cfg(windows)]
    {
        // `echo(` (rather than `echo `) prints an empty line for an empty
        // argv instead of `ECHO is off.`.
        let mut script = format!("@echo off\r\n>>\"{}\" echo(%*\r\n", log_path.display());
        let emit = |script: &mut String, arm: &ShimArm<'_>, idx: usize| {
            for (stream, body, redirect) in [("out", arm.stdout, ""), ("err", arm.stderr, " 1>&2")]
            {
                if body.is_empty() {
                    continue;
                }
                let side = dir.join(format!("{name}.{idx}.{stream}"));
                std::fs::write(&side, body).expect("write shim output");
                script.push_str(&format!("type \"{}\"{redirect}\r\n", side.display()));
            }
            script.push_str(&format!("exit /b {}\r\n", arm.exit_code));
        };

        let mut answered = false;
        for (idx, arm) in arms.iter().enumerate() {
            if arm.matches.is_empty() {
                emit(&mut script, arm, idx);
                answered = true;
                break;
            }
            script.push_str(&format!(
                "echo(%*| findstr /C:\"{}\" >nul\r\nif errorlevel 1 goto cfgd_arm{idx}\r\n",
                arm.matches
            ));
            emit(&mut script, arm, idx);
            script.push_str(&format!(":cfgd_arm{idx}\r\n"));
        }
        if !answered {
            script.push_str("exit /b 0\r\n");
        }

        let bin_path = dir.join(format!("{name}.cmd"));
        std::fs::write(&bin_path, script).expect("write shim");
        bin_path
    }
}

/// Owns a tempdir holding a [`write_tool_shim`] stand-in plus the env-vars
/// that route a single `tool_cmd(env_var, default)` factory at it. The shim
/// records its full argv to a log file and exits with a chosen status,
/// optionally writing canned stdout/stderr.
///
/// Construct with [`ToolShim::install`]. Drops the env-vars and tempdir on
/// drop, even when a test panics — env state never leaks across tests.
///
/// Cross-platform: a `/bin/sh` script on Unix, a `.cmd` batch file on Windows.
pub struct ToolShim {
    _tmp: tempfile::TempDir,
    env_var: String,
    log_path: std::path::PathBuf,
}

impl ToolShim {
    /// Install a shim that records argv to a log and exits with `exit_code`,
    /// emitting `stdout` to stdout and `stderr` to stderr. The shim is pointed
    /// at by the `env_var` env-var (the same var read by `tool_cmd`).
    ///
    /// Implementation detail: the log path is baked into the shim script, so
    /// only this shim's own invocations can land in it — routing it through an
    /// env var let any concurrently-invoked shim append to whichever log was
    /// installed last. `argv` is appended one line per invocation so
    /// multi-call tests can assert ordering.
    pub fn install(env_var: &str, exit_code: i32, stdout: &str, stderr: &str) -> Self {
        Self::with_arms(env_var, &[ShimArm::always(stdout, stderr, exit_code)])
    }

    /// Install a shim that exits non-zero (emitting `stderr`) **only** when its
    /// joined argv contains `fail_substr`, and exits 0 otherwise. Records argv
    /// like [`ToolShim::install`]. Use to exercise batch-then-per-package
    /// fallbacks where one package in a batch is invalid but the rest are valid.
    pub fn install_failing_on(env_var: &str, fail_substr: &str, stderr: &str) -> Self {
        Self::with_arms(
            env_var,
            &[
                ShimArm {
                    matches: fail_substr,
                    stdout: "",
                    stderr,
                    exit_code: 1,
                },
                ShimArm::always("", "", 0),
            ],
        )
    }

    fn with_arms(env_var: &str, arms: &[ShimArm<'_>]) -> Self {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let bin_path = write_tool_shim(tmp.path(), &format!("shim-{env_var}"), arms);

        // SAFETY: callers wrap with `serial_test::serial`, so no concurrent
        // reader observes a mid-update env state.
        unsafe {
            std::env::set_var(env_var, &bin_path);
        }

        Self {
            log_path: tmp.path().join("argv.log"),
            _tmp: tmp,
            env_var: env_var.to_string(),
        }
    }

    /// Read the captured argv. Each line is the space-joined argv of one
    /// invocation, in order.
    ///
    /// `cmd` hands a batch file its arguments individually quoted, so the
    /// Windows log is stripped of `"` to read as the same unquoted join
    /// `"$*"` gives on Unix.
    pub fn argv_log(&self) -> String {
        // absent-file-ok: a shim nothing ran wrote no log, so an empty argv is honest.
        let raw = std::fs::read_to_string(&self.log_path).unwrap_or_default();
        #[cfg(windows)]
        return raw.replace('"', "");
        #[cfg(not(windows))]
        raw
    }

    /// Number of times the shim was invoked.
    pub fn invocation_count(&self) -> usize {
        self.argv_log().lines().filter(|l| !l.is_empty()).count()
    }

    /// The captured argv lines that name `subject`, in order.
    ///
    /// The seam this shim installs is an ENV VAR, which is process-global and
    /// carries no exclusive guard — so any test running in parallel that spawns
    /// the same tool lands in this log too, whatever `serial_test` group the
    /// asserting test is in (`serial` excludes only other serial tests). A
    /// spawn-count claim is always about one subject — one registry key, one
    /// schema, one domain — so filter to the lines naming it rather than
    /// asserting on a log another test also writes to.
    pub fn argv_lines_naming(&self, subject: &str) -> Vec<String> {
        self.argv_log()
            .lines()
            .filter(|l| l.contains(subject))
            .map(str::to_string)
            .collect()
    }
}

impl Drop for ToolShim {
    fn drop(&mut self) {
        // SAFETY: see `install`.
        unsafe {
            std::env::remove_var(&self.env_var);
        }
    }
}

/// Holds the exclusive spawn-environment guard while a shim directory sits at
/// the front of `PATH`, and restores the prior `PATH` on drop. Prepending a
/// directory containing a fake `bash` / `curl` / `sudo` is a process-global
/// mutation: a parallel test resolving the same name would silently get the
/// shim, so the window must exclude concurrent spawns exactly like an
/// empty-`PATH` window does.
#[cfg(unix)]
pub struct PathShimGuard {
    _path: EnvVarGuard,
    _spawn_excl: ExclusiveEnvGuard,
}

#[cfg(unix)]
impl PathShimGuard {
    fn prepend(dir: &Path) -> Self {
        let spawn_excl = path_env_mutation_guard();
        let old_path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{}:{}", dir.display(), old_path);
        Self {
            _path: EnvVarGuard::set("PATH", &new_path),
            _spawn_excl: spawn_excl,
        }
    }
}

/// Install a tempdir-scoped shim script named `binary` (`bash`, `curl`,
/// `powershell`, etc.) at the FRONT of `PATH`. Returns a tuple whose first
/// element pins the tempdir alive for the test's lifetime and whose second
/// restores the prior PATH on drop (see [`PathShimGuard`]). Use for production
/// code that invokes a bare-name binary via `Command::new("<binary>")` (no
/// env-var seam).
///
/// `exit_code` is the shim's exit; `stdout`/`stderr` are written verbatim
/// (with embedded `"` shell-escaped). Caller is responsible for the
/// `#[serial]` gate — PATH mutation is process-global.
#[cfg(unix)]
pub fn install_named_path_shim(
    binary: &str,
    exit_code: u8,
    stdout: &str,
    stderr: &str,
) -> (tempfile::TempDir, PathShimGuard) {
    use std::os::unix::fs::PermissionsExt;
    let bin_dir = tempfile::tempdir().expect("tempdir");
    let script = format!(
        "#!/bin/sh\nprintf '%s' \"{}\"\nprintf '%s' \"{}\" >&2\nexit {}\n",
        stdout.replace('"', "\\\""),
        stderr.replace('"', "\\\""),
        exit_code
    );
    let path = bin_dir.path().join(binary);
    std::fs::write(&path, script).expect("write shim");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let guard = PathShimGuard::prepend(bin_dir.path());
    (bin_dir, guard)
}

/// The argv record of a [`install_named_path_shim_logged`] shim. Same two
/// readers as [`ToolShim`], because a PATH-resolved manager has exactly the
/// same question asked of it — "what did it actually run, and how often" — and
/// a test that can only see the shim's exit code cannot tell a refresh that
/// updated an index from one that upgraded every installed package.
#[cfg(unix)]
pub struct PathShimLog {
    log_path: std::path::PathBuf,
}

#[cfg(unix)]
impl PathShimLog {
    /// Read the captured argv. Each line is the space-joined argv of one
    /// invocation, in order.
    pub fn argv_log(&self) -> String {
        // absent-file-ok: a shim nothing ran wrote no log.
        std::fs::read_to_string(&self.log_path).unwrap_or_default()
    }

    /// Number of times the shim was invoked.
    pub fn invocation_count(&self) -> usize {
        self.argv_log().lines().filter(|l| !l.is_empty()).count()
    }
}

/// [`install_named_path_shim`] that also records every invocation's argv.
///
/// Reach for it over the plain variant whenever the assertion is about WHICH
/// subcommand ran rather than about what the manager did with its output — the
/// difference between `scoop update` and `scoop update *`, or between a
/// no-index manager refreshing nothing and one silently running
/// `winget upgrade --all`, is invisible in an exit code.
#[cfg(unix)]
pub fn install_named_path_shim_logged(
    binary: &str,
    exit_code: u8,
    stdout: &str,
    stderr: &str,
) -> (tempfile::TempDir, PathShimGuard, PathShimLog) {
    use std::os::unix::fs::PermissionsExt;
    let bin_dir = tempfile::tempdir().expect("tempdir");
    let log_path = bin_dir.path().join("argv.log");
    // The log path is baked into the script rather than read from the
    // environment, so no OTHER shim can write to it. Invocations of THIS name
    // by a concurrently-running test still can — the shim is on the
    // process-global PATH — so a test asserting on the log must be `serial`
    // along with every other test that spawns the same binary.
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nprintf '%s' \"{}\"\nprintf '%s' \"{}\" >&2\nexit {}\n",
        log_path.display().to_string().replace('\'', "'\\''"),
        stdout.replace('"', "\\\""),
        stderr.replace('"', "\\\""),
        exit_code
    );
    let path = bin_dir.path().join(binary);
    std::fs::write(&path, script).expect("write shim");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let guard = PathShimGuard::prepend(bin_dir.path());
    (bin_dir, guard, PathShimLog { log_path })
}

/// Install several `#!/bin/sh` shims into a single tempdir prepended to PATH.
/// Each `(name, exit_code)` becomes a 0o755 script that exits with the given
/// code (no stdout/stderr). Returns `(TempDir, PathShimGuard)` whose drops
/// release the temp directory and restore the prior PATH. Use for tests whose
/// production code-path invokes multiple bare-name binaries (`useradd`, `sudo`,
/// `bash` etc.) where a single-binary shim is insufficient. Caller is
/// responsible for the `#[serial]` gate — PATH mutation is process-global.
#[cfg(unix)]
pub fn install_named_path_shims(shims: &[(&str, i32)]) -> (tempfile::TempDir, PathShimGuard) {
    use std::os::unix::fs::PermissionsExt;
    let bin_dir = tempfile::tempdir().expect("tempdir");
    for (name, exit_code) in shims {
        let path = bin_dir.path().join(name);
        std::fs::write(&path, format!("#!/bin/sh\nexit {exit_code}\n")).expect("write shim");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    let guard = PathShimGuard::prepend(bin_dir.path());
    (bin_dir, guard)
}

// ---------------------------------------------------------------------------
// PATH-mutation / interpreter-spawn coordination
// ---------------------------------------------------------------------------

/// Serializes the process-global *spawn environment* — `PATH` and the working
/// directory — against every process spawn in the test binary.
///
/// reader is a data race on the C `environ`. Two kinds of code read `PATH`:
/// resolution ([`crate::command_path`] and everything over it —
/// `command_available`, `require_tool`) and any spawn that resolves its program
/// through `PATH` — a script's interpreter (`sh`/`bash`), or `git` via
/// [`crate::git_cmd_local`] / [`crate::git_cmd_safe`]. A test that empties
/// `PATH` to drive a "command not found" branch races and corrupts either. It
/// surfaces as a spurious `could not spawn the script interpreter (os error 2)`,
/// as a `git … must succeed` assertion that fails on one arbitrary git call out
/// of several, or as an unrelated `require_tool("sh")` reporting sh missing.
///
/// `#[serial]` cannot close this: it only excludes other `#[serial]` tests,
/// never the non-serial reader majority. Nor does `nextest` expose it — its
/// process-per-test model gives every test its own `environ`, so this races only
/// under `cargo test`'s thread-per-test model (the shape CI runs on macOS).
/// This lock guards the real resource boundary instead — readers take the shared
/// read guard ([`path_env_read_guard`]), PATH mutation takes the exclusive write
/// guard ([`path_env_mutation_guard`], also taken by [`CwdGuard`] and
/// [`PathShimGuard`]) — so the two can never overlap. Readers run fully
/// parallel with each other; only an active mutation window blocks them.
///
/// Both halves are re-entrant *per thread*, tracked by the thread-locals below.
/// Two shapes they cannot cover. Cross-thread: a thread holding the exclusive
/// guard that waits on a helper thread which spawns (a raw `spawn_blocking`,
/// say) deadlocks, because the helper has neither flag — keep a mutation window
/// on one thread. And shared-then-exclusive on one thread: a read guard cannot
/// upgrade to a write guard, so [`path_env_mutation_guard`] `debug_assert!`s
/// that no shared guard is held rather than silently allowing the mutation.
///
/// ## Why this is not a `std::sync::RwLock`
///
/// `RwLock` is write-preferring: once a writer is queued, a reader arriving
/// after it waits even though the lock is ALREADY held for reading. That
/// starves the one pattern concurrent dispatch is made of — a reader that
/// cannot leave its critical section until a SECOND reader enters it. A lane
/// worker blocked in a fixture rendezvous holds the read side; the thread
/// waiting on it cannot proceed until the sibling worker starts; the sibling is
/// a fresh thread, so it takes a real read and parks behind whatever writer
/// happened to queue in between. Nothing in that cycle can move, and the only
/// thing that ends it is a test-side timeout expiring minutes later — after
/// stalling every other reader in the binary behind the same writer.
///
/// So admission is: a reader waits while a writer HOLDS the gate, and while a
/// writer is waiting for a gate no reader holds. A writer that is waiting on
/// readers already inside does not block another reader from joining them — it
/// has to wait for those readers regardless, and refusing the newcomer buys it
/// nothing while making the deadlock above representable. Writers still make
/// progress: readers stop being admitted the moment the reader count reaches
/// zero with a writer waiting.
static PATH_ENV_LOCK: PathEnvGate = PathEnvGate {
    state: Mutex::new(PathEnvGateState {
        readers: 0,
        readers_waiting: Vec::new(),
        writer: false,
        writers_waiting: 0,
    }),
    signal: Condvar::new(),
};

/// The `PATH` gate's admission state and the condvar every waiter parks on.
struct PathEnvGate {
    state: Mutex<PathEnvGateState>,
    signal: Condvar,
}

struct PathEnvGateState {
    readers: usize,
    /// The threads parked in [`PathEnvGate::acquire_read`], by identity rather
    /// than by count: a claim that a PARTICULAR resolution queued cannot be
    /// answered by a stranger's reader arriving in the same window, and a
    /// count is all a stranger has to satisfy. Never more than a handful of
    /// entries, so a linear scan beats anything ordered or hashed.
    readers_waiting: Vec<std::thread::ThreadId>,
    writer: bool,
    writers_waiting: usize,
}

impl PathEnvGate {
    fn locked(&self) -> std::sync::MutexGuard<'_, PathEnvGateState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn acquire_read(&self) {
        let mut state = self.locked();
        let me = std::thread::current().id();
        let mut queued = false;
        while state.writer || (state.writers_waiting > 0 && state.readers == 0) {
            if !queued {
                state.readers_waiting.push(me);
                queued = true;
                // Announced from the PARKING path alone, exactly as the write
                // half announces its own arrival, so a test can observe the
                // queued reader rather than sleep a guess at when it arrives.
                // A reader admitted straight away never releases the mutex in
                // between, so the entry it does not park on is unobservable —
                // and waking every waiter for it would wake a parked writer
                // for a predicate a reader admission can only have worsened,
                // on the hottest call in the test binary.
                self.signal.notify_all();
            }
            state = self
                .signal
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        if queued && let Some(at) = state.readers_waiting.iter().position(|id| *id == me) {
            state.readers_waiting.swap_remove(at);
        }
        state.readers += 1;
    }

    fn release_read(&self) {
        let mut state = self.locked();
        // An underflow here means a release without a matching acquire — a bug
        // in the gate itself, not a count to saturate through.
        debug_assert!(
            state.readers > 0,
            "PATH gate: release_read with readers == 0"
        );
        state.readers -= 1;
        if state.readers == 0 {
            self.signal.notify_all();
        }
    }

    fn acquire_write(&self) {
        let mut state = self.locked();
        state.writers_waiting += 1;
        // Announced before parking, so a test can observe the queued writer
        // rather than sleep a guess at when it arrives.
        self.signal.notify_all();
        // A generous bound no legitimate suite run can approach: a silent
        // hang points at nothing, so a writer that waits this long panics
        // with a diagnostic naming the gate instead of leaving the suite to
        // time out with no pointer to why.
        const WRITER_STARVATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
        let (next, result) = self
            .signal
            .wait_timeout_while(state, WRITER_STARVATION_TIMEOUT, |gate| {
                gate.writer || gate.readers > 0
            })
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state = next;
        if result.timed_out() {
            // Dropped from the queue before the panic: `locked()` swallows the
            // poison, so a count left standing here would park every later
            // reader on an idle gate forever — a silent hang in place of the
            // diagnostic this panic exists to print.
            state.writers_waiting -= 1;
            panic!(
                "PATH_ENV_LOCK: writer starved for over {:?} with {} reader(s) still \
                 holding the gate — this is writer starvation, not a legitimate wait",
                WRITER_STARVATION_TIMEOUT, state.readers
            );
        }
        state.writers_waiting -= 1;
        state.writer = true;
    }

    fn release_write(&self) {
        let mut state = self.locked();
        state.writer = false;
        self.signal.notify_all();
    }
}

/// Block until a thread is waiting to take [`path_env_mutation_guard`]'s
/// exclusive side, answering `false` if none arrives within `timeout`.
///
/// The observable a concurrency test needs to reach "a writer is queued"
/// without a clock standing in for it: a test that slept instead would pass
/// vacuously whenever the sleep were short, and prove nothing about the
/// admission rule it is there to pin.
pub fn await_queued_path_writer(timeout: std::time::Duration) -> bool {
    let state = PATH_ENV_LOCK.locked();
    let (state, _) = PATH_ENV_LOCK
        .signal
        .wait_timeout_while(state, timeout, |gate| gate.writers_waiting == 0)
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.writers_waiting > 0
}

/// [`await_queued_path_writer`]'s twin over the read half: block until
/// `thread` is waiting to take [`path_env_read_guard`], answering `false` if
/// it has not queued within `timeout`.
///
/// What proves a `PATH` reader QUEUED on a held mutation window rather than
/// answering from inside it. The queue is the whole claim — the answer a
/// reader would have given from inside the window is a machine with nothing
/// on it — and a sleep in its place would pass whenever it happened to be
/// long enough, proving nothing about the admission rule.
///
/// Named by THREAD, not by count: `#[serial]` excludes only other serial
/// tests, so any reader in a 16-thread run satisfies "somebody is queued", and
/// a pin answered by a stranger's reader goes green on the very regression it
/// stands for. Take the id off the `JoinHandle` of the thread whose park is
/// the claim.
pub fn await_queued_path_reader(
    thread: std::thread::ThreadId,
    timeout: std::time::Duration,
) -> bool {
    let state = PATH_ENV_LOCK.locked();
    let (state, _) = PATH_ENV_LOCK
        .signal
        .wait_timeout_while(state, timeout, |gate| {
            !gate.readers_waiting.contains(&thread)
        })
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.readers_waiting.contains(&thread)
}

thread_local! {
    /// Depth of nested [`path_env_read_guard`] acquisitions on this thread.
    static SPAWN_GUARD_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Whether this thread holds the exclusive guard.
    static SPAWN_GUARD_EXCLUSIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Shared read guard held across a read of `PATH`. Acquired at the top of
/// `reconciler::scripts::execute_script`, inside the `git` command factories
/// and inside `command_path` itself, so a script-spawning, git-spawning or
/// command-resolving test is covered by the seam it goes through: a test that
/// asserts a *successful* `command_path` / `command_available` /
/// `require_tool` resolution needs no guard of its own. A test asserting a
/// resolution *fails* never did — an empty `PATH` cannot turn a miss into a
/// hit.
///
/// Re-entrant by design: a `CwdGuard`/`PathShimGuard` window (which hold the
/// exclusive guard) composing with a spawn inside it is normal, and a thread
/// that already holds the exclusive side must not queue behind itself. A nested
/// acquisition, and any acquisition on a thread already holding the exclusive
/// guard, is therefore a no-op. See `PATH_ENV_LOCK`.
// env-mutator-ok: takes cfgd's PATH lock and sets a thread-local Cell; writes no env var.
pub fn path_env_read_guard() -> SpawnEnvGuard {
    if SPAWN_GUARD_EXCLUSIVE.with(std::cell::Cell::get)
        || SPAWN_GUARD_DEPTH.with(std::cell::Cell::get) > 0
    {
        return SpawnEnvGuard(None);
    }
    PATH_ENV_LOCK.acquire_read();
    SPAWN_GUARD_DEPTH.with(|d| d.set(1));
    SpawnEnvGuard(Some(()))
}

/// Whether THIS thread currently holds [`path_env_mutation_guard`]'s exclusive
/// guard. For a coordinator (`reconciler::lanes::dispatch_package_lanes`, say)
/// about to spawn helper threads: a helper carries neither of this lock's
/// thread-locals — they are per-thread, and a freshly spawned thread starts
/// with both at their default — so a helper's own [`path_env_read_guard`]
/// genuinely blocks on `PATH_ENV_LOCK` rather than short-circuiting as a
/// re-entrant no-op, and never unblocks if the exclusive holder is the same
/// thread that is now waiting on the helper. Check this BEFORE spawning, so
/// that precondition fails fast instead of hanging.
pub fn path_env_exclusive_guard_held() -> bool {
    SPAWN_GUARD_EXCLUSIVE.with(std::cell::Cell::get)
}

/// Shared read guard returned by [`path_env_read_guard`]. `None` for a
/// re-entrant acquisition, which took nothing and must release nothing.
pub struct SpawnEnvGuard(Option<()>);

impl Drop for SpawnEnvGuard {
    fn drop(&mut self) {
        if self.0.is_some() {
            SPAWN_GUARD_DEPTH.with(|d| d.set(0));
            PATH_ENV_LOCK.release_read();
        }
    }
}

/// Exclusive write guard for a test that empties the process-global `PATH` to
/// exercise a command-not-found branch, or mutates the working directory
/// (which [`CwdGuard`] and [`PathShimGuard`] do for you). Declare it *before*
/// the `EnvVarGuard` that mutates `PATH` so it drops last, bracketing the
/// entire mutation window. Never spawn a script or a `git` child while holding
/// it directly — that both contradicts an empty-`PATH` test (nothing resolves)
/// and, absent the re-entrancy below, risks a same-thread read-after-write
/// deadlock.
///
/// Re-entrant per thread, exactly like [`path_env_read_guard`]: combining a
/// [`CwdGuard`] with a [`PathShimGuard`], or nesting either, is a natural
/// thing for a test to do and must not queue the gate against itself. The
/// inner acquisitions are no-ops and the gate is released
/// when the outermost guard drops. See `PATH_ENV_LOCK` for the cross-thread
/// limit.
///
/// The one order that cannot be made re-entrant is shared-then-exclusive: a
/// thread holding [`path_env_read_guard`]'s read guard cannot upgrade to the
/// write guard, and degrading to a no-op would be worse than the hang it
/// avoids — it would let a `PATH`/cwd mutation run while the in-flight spawn
/// this lock exists to protect is still resolving its program. Take the
/// exclusive guard first, or not inside a spawn.
// env-mutator-ok: takes cfgd's PATH lock; it guards a caller's write, never performs one.
pub fn path_env_mutation_guard() -> ExclusiveEnvGuard {
    debug_assert!(
        SPAWN_GUARD_DEPTH.with(std::cell::Cell::get) == 0,
        "path_env_mutation_guard() taken while holding the shared spawn guard: \
         a read guard cannot upgrade to a write guard, so this deadlocks. \
         Take the exclusive guard before the spawn, not during it."
    );
    if SPAWN_GUARD_EXCLUSIVE.with(std::cell::Cell::get) {
        return ExclusiveEnvGuard { held: false };
    }
    PATH_ENV_LOCK.acquire_write();
    SPAWN_GUARD_EXCLUSIVE.with(|f| f.set(true));
    ExclusiveEnvGuard { held: true }
}

/// Exclusive spawn-environment guard returned by [`path_env_mutation_guard`].
pub struct ExclusiveEnvGuard {
    /// `false` for a re-entrant acquisition, which took nothing.
    held: bool,
}

impl Drop for ExclusiveEnvGuard {
    fn drop(&mut self) {
        if self.held {
            SPAWN_GUARD_EXCLUSIVE.with(|f| f.set(false));
            PATH_ENV_LOCK.release_write();
        }
    }
}

/// RAII guard that snapshots the process-global bootstrapped-PATH registry on
/// construction and restores it on drop.
///
/// The registry that `crate::register_bootstrapped_path_dirs` feeds is never
/// cleared — in production a bootstrap that happened cannot un-happen. In a test
/// binary that makes every registration permanent for every test that runs
/// after it, and `command_path` searches those directories once `$PATH` misses.
/// A fixture registering a real host directory therefore changes what unrelated
/// later tests can resolve, and only on hosts where that directory exists:
/// registering `/opt/homebrew/bin` made an empty-`PATH` "git is missing" test
/// find git on macOS and not on Linux. Take this guard in any fixture that
/// drives a bootstrap so the registration cannot outlive it.
pub struct BootstrappedPathDirsGuard {
    prior: Vec<std::path::PathBuf>,
}

impl BootstrappedPathDirsGuard {
    /// Snapshot the currently registered directories.
    pub fn capture() -> Self {
        Self {
            prior: crate::bootstrapped_path_dirs(),
        }
    }

    /// Snapshot the registry and empty it for the guard's lifetime. Use in a
    /// test asserting a "command not found" branch: emptying `PATH` alone does
    /// not make a command unresolvable, because this registry is searched after
    /// it.
    pub fn capture_and_clear() -> Self {
        let guard = Self::capture();
        crate::restore_bootstrapped_path_dirs(Vec::new());
        guard
    }
}

impl Default for BootstrappedPathDirsGuard {
    fn default() -> Self {
        Self::capture()
    }
}

impl Drop for BootstrappedPathDirsGuard {
    fn drop(&mut self) {
        crate::restore_bootstrapped_path_dirs(std::mem::take(&mut self.prior));
    }
}

/// Run `measure` inside a window in which nothing else moved the process-wide
/// resolution generation (`crate::command_resolution_generation`), retrying with
/// a fresh call until it gets one.
///
/// Every memo-hit claim — "the sweep ran once", "the memoized miss still
/// stands" — is only measurable while that generation holds still, and any test
/// in the binary that runs a mock install or a lifecycle script bumps it for
/// everyone. Without this the assertion is not wrong, it is unmeasurable, and it
/// fails at random on a loaded runner. `serial_test::serial` cannot substitute:
/// it excludes only other serial tests, not the parallel majority.
///
/// `measure` must be re-runnable — build the registry, counters and probe files
/// it observes INSIDE the closure, so a retry measures a fresh subject rather
/// than one an abandoned attempt already warmed.
pub fn measured_in_a_stable_generation<T>(mut measure: impl FnMut() -> T) -> T {
    for _ in 0..64 {
        let before = crate::command_resolution_generation();
        let measured = measure();
        if crate::command_resolution_generation() == before {
            return measured;
        }
    }
    panic!(
        "the resolution generation never held still across a measurement — \
         something in this binary is invalidating it continuously"
    );
}

/// RAII pin of the `command_path` memo's TTL, restoring the prior setting on
/// drop.
///
/// The memo expires an entry after 30 seconds so a weeks-long daemon notices a
/// binary a human installed by hand. That ceiling is invisible to production
/// and load-bearing for it, but it is wall time, and a test asserting either
/// that a memoized answer STANDS or that it EXPIRES would otherwise be asserting
/// about how long two adjacent statements took on a loaded runner. Pin it and
/// the claim is about the mechanism.
///
/// Pinning is process-global and needs no serialization of its own: a longer TTL
/// only lets another test's entries live longer, and a zero TTL only makes them
/// recompute — neither can change the ANSWER any concurrent test reads.
pub struct CommandPathMemoTtlGuard {
    prior: Option<u64>,
}

impl CommandPathMemoTtlGuard {
    /// Pin the TTL to `ttl`, saturating at the millisecond range.
    pub fn pinned(ttl: std::time::Duration) -> Self {
        // `u64::MAX` is the "no override" sentinel, so a pin that would land on
        // it saturates one below: `pinned(Duration::MAX)` must pin the ceiling
        // out of reach, never silently restore the default it was called to
        // displace.
        let millis = u64::try_from(ttl.as_millis())
            .unwrap_or(u64::MAX)
            .min(u64::MAX - 1);
        Self {
            prior: crate::set_command_path_memo_ttl_override(Some(millis)),
        }
    }

    /// Pin the TTL beyond any test's lifetime, so no memoized entry can expire
    /// mid-test. For a test whose claim is that an answer still stands.
    pub fn never_expires() -> Self {
        Self::pinned(std::time::Duration::from_millis(u64::MAX - 1))
    }

    /// Pin the TTL to zero, so every entry is expired the moment it is stored.
    /// For a test whose claim is that expiry retires an answer.
    pub fn always_expired() -> Self {
        Self::pinned(std::time::Duration::ZERO)
    }
}

impl Drop for CommandPathMemoTtlGuard {
    fn drop(&mut self) {
        crate::set_command_path_memo_ttl_override(self.prior);
    }
}

/// RAII pin of the rate-limited retry ladder's first step, restoring the prior
/// setting on drop. The sibling of [`CommandPathMemoTtlGuard`], for a different
/// reason: the ladder a 429 moves the gateway client onto is measured in seconds
/// against the gateway's own enrollment quota, so a test proving that the client
/// CHOSE that ladder (rather than the half-second transient one) would otherwise
/// have to sleep the real wait to see it.
///
/// Pinning this one needs serialization: a test asserting that the UNPINNED
/// accessor still answers the constant is measuring exactly what a concurrent pin
/// displaces. Pair every use with `#[serial_test::serial(rate_limited_backoff)]`,
/// the named group that assertion shares — named, so nothing else is held up.
pub struct RateLimitedBackoffGuard {
    prior: Option<u64>,
}

impl RateLimitedBackoffGuard {
    /// Pin the first step of the rate-limited ladder to `step`, saturating at
    /// the millisecond range. `u64::MAX` is the "no override" sentinel, so a pin
    /// that would land on it saturates one below rather than silently restoring
    /// the default it was called to displace.
    pub fn pinned(step: std::time::Duration) -> Self {
        let millis = u64::try_from(step.as_millis())
            .unwrap_or(u64::MAX)
            .min(u64::MAX - 1);
        Self {
            prior: crate::retry::set_rate_limited_backoff_override(Some(millis)),
        }
    }
}

impl Drop for RateLimitedBackoffGuard {
    fn drop(&mut self) {
        crate::retry::set_rate_limited_backoff_override(self.prior);
    }
}

/// RAII pin of the installed-package enumeration memo's TTL, restoring the
/// prior setting on drop. The sibling of [`CommandPathMemoTtlGuard`], for the
/// same reason and with the same three constructors: the enumeration memo also
/// carries a 30-second ceiling, so a holder that outlives one unit of work
/// (the MCP server) re-asks after a human installs something cfgd did not.
///
/// Unlike its sibling, pinning this one DOES need serialization: every test
/// that reads the enumeration memo asserts on the COUNT of enumerations rather
/// than on the answer, so a concurrent zero pin makes another test's memoized
/// listing recompute and changes exactly what that test measures. Pair every
/// use with `#[serial_test::serial(enumeration_memo)]`, the named group the
/// enumeration-count tests share — named, so nothing outside them is held up.
///
/// Scope is the test BINARY, since the override atomic is process-global and a
/// binary is a process, and every pin in either binary carries the group —
/// `every_test_pinning_a_serialized_seam_joins_its_own_group` reads this seam
/// off its own roster and fails any declaration that pins it, or that asserts
/// on the unpinned ceiling, without the attribute. The group is demanded
/// mechanically rather than by this paragraph.
pub struct EnumerationMemoTtlGuard {
    prior: Option<u64>,
}

impl EnumerationMemoTtlGuard {
    /// Pin the TTL to `ttl`, saturating at the millisecond range.
    pub fn pinned(ttl: std::time::Duration) -> Self {
        // `u64::MAX` is the "no override" sentinel, so a pin that would land on
        // it saturates one below: `pinned(Duration::MAX)` must pin the ceiling
        // out of reach, never silently restore the default it was called to
        // displace.
        let millis = u64::try_from(ttl.as_millis())
            .unwrap_or(u64::MAX)
            .min(u64::MAX - 1);
        Self {
            prior: crate::providers::set_enumeration_memo_ttl_override(Some(millis)),
        }
    }

    /// Pin the TTL beyond any test's lifetime, so no memoized enumeration can
    /// expire mid-test. For a test whose claim is that an answer still stands.
    pub fn never_expires() -> Self {
        Self::pinned(std::time::Duration::from_millis(u64::MAX - 1))
    }

    /// Pin the TTL to zero, so every enumeration is expired the moment it is
    /// stored. For a test whose claim is that expiry retires one.
    pub fn always_expired() -> Self {
        Self::pinned(std::time::Duration::ZERO)
    }
}

impl Drop for EnumerationMemoTtlGuard {
    fn drop(&mut self) {
        crate::providers::set_enumeration_memo_ttl_override(self.prior);
    }
}

/// RAII pin of the provider-availability sweep's TTL, restoring the prior
/// setting on drop. The sibling of [`EnumerationMemoTtlGuard`] over the sweep a
/// `ProviderRegistry` memoizes: that one memoizes what a manager HAS, this one
/// whether the manager is on the machine at all.
///
/// The ceiling exists for the holder that outlives one run — the daemon keeps
/// one registry across ticks — so a test whose claim is that a sweep still
/// stands pins `never_expires`, and one whose claim is that the ceiling retires
/// a sweep pins `always_expired`. Pair every use with the UNNAMED
/// `#[serial_test::serial]`, which is the group the sweep's own tests already
/// share: the pin is process-global and every test that reads this memo asserts
/// on a COUNT of `is_available` probes, and a named group would not exclude the
/// unnamed ones.
pub struct AvailabilityMemoTtlGuard {
    prior: Option<u64>,
}

impl AvailabilityMemoTtlGuard {
    /// Pin the TTL to `ttl`, saturating at the millisecond range.
    pub fn pinned(ttl: std::time::Duration) -> Self {
        let millis = u64::try_from(ttl.as_millis())
            .unwrap_or(u64::MAX)
            .min(u64::MAX - 1);
        Self {
            prior: crate::providers::set_availability_memo_ttl_override(Some(millis)),
        }
    }

    /// Pin the TTL beyond any test's lifetime. For a test whose claim is that a
    /// sweep still stands.
    pub fn never_expires() -> Self {
        Self::pinned(std::time::Duration::from_millis(u64::MAX - 1))
    }

    /// Pin the TTL to zero, so every sweep is expired the moment it is stored.
    /// For a test whose claim is that expiry retires one.
    pub fn always_expired() -> Self {
        Self::pinned(std::time::Duration::ZERO)
    }
}

impl Drop for AvailabilityMemoTtlGuard {
    fn drop(&mut self) {
        crate::providers::set_availability_memo_ttl_override(self.prior);
    }
}

/// RAII pin of the available-version memo's TTL, restoring the prior setting on
/// drop. The sibling of [`EnumerationMemoTtlGuard`] over the other half of the
/// package-manager memo pair: that one memoizes what a manager HAS, this one
/// what it OFFERS.
///
/// Pinning needs the same serialization for the same reason — every test that
/// reads this memo asserts on the COUNT of version queries, so a concurrent zero
/// pin makes another test's memoized offer recompute and changes exactly what
/// that test measures. Pair every use with
/// `#[serial_test::serial(available_version_memo)]`.
pub struct AvailableVersionMemoTtlGuard {
    prior: Option<u64>,
}

impl AvailableVersionMemoTtlGuard {
    /// Pin the TTL to `ttl`, saturating at the millisecond range.
    pub fn pinned(ttl: std::time::Duration) -> Self {
        // `u64::MAX` is the "no override" sentinel, so a pin that would land on
        // it saturates one below: `pinned(Duration::MAX)` must pin the ceiling
        // out of reach, never silently restore the default it was called to
        // displace.
        let millis = u64::try_from(ttl.as_millis())
            .unwrap_or(u64::MAX)
            .min(u64::MAX - 1);
        Self {
            prior: crate::providers::set_available_version_memo_ttl_override(Some(millis)),
        }
    }

    /// Pin the TTL beyond any test's lifetime, so no memoized offer can expire
    /// mid-test. For a test whose claim is that an answer still stands.
    pub fn never_expires() -> Self {
        Self::pinned(std::time::Duration::from_millis(u64::MAX - 1))
    }

    /// Pin the TTL to zero, so every offer is expired the moment it is stored.
    /// For a test whose claim is that expiry retires one.
    pub fn always_expired() -> Self {
        Self::pinned(std::time::Duration::ZERO)
    }
}

impl Drop for AvailableVersionMemoTtlGuard {
    fn drop(&mut self) {
        crate::providers::set_available_version_memo_ttl_override(self.prior);
    }
}

/// RAII pin of the daemon tick cache's config-reuse ceiling, restoring the
/// prior setting on drop. The sibling of [`AvailableVersionMemoTtlGuard`] for
/// the backstop that bounds how long ONE config derivation may be reused when
/// its recorded inputs all still stand.
///
/// The real ceiling is five minutes, so the expiry filter it guards is
/// unreachable from a test that does not pin it. Pair every use with
/// `#[serial_test::serial(tick_cache_reuse)]`: the pin is process-global and
/// every test that reads this cache asserts on a COUNT of derivations, which is
/// exactly what a concurrent zero pin changes.
pub struct ConfigReuseMaxAgeGuard {
    prior: Option<u64>,
}

impl ConfigReuseMaxAgeGuard {
    /// Pin the ceiling to `ttl`, saturating at the millisecond range.
    pub fn pinned(ttl: std::time::Duration) -> Self {
        // `u64::MAX` is the "no override" sentinel, so a pin that would land on
        // it saturates one below rather than silently restoring the default.
        let millis = u64::try_from(ttl.as_millis())
            .unwrap_or(u64::MAX)
            .min(u64::MAX - 1);
        Self {
            prior: crate::daemon::tick_cache::set_config_reuse_max_age_override(Some(millis)),
        }
    }

    /// Pin the ceiling beyond any test's lifetime, for a test whose claim is
    /// that a derivation still stands.
    pub fn never_expires() -> Self {
        Self::pinned(std::time::Duration::from_millis(u64::MAX - 1))
    }

    /// Pin the ceiling to zero, so every derivation is stale the moment it is
    /// stored. For a test whose claim is that the ceiling retires one.
    pub fn always_expired() -> Self {
        Self::pinned(std::time::Duration::ZERO)
    }
}

impl Drop for ConfigReuseMaxAgeGuard {
    fn drop(&mut self) {
        crate::daemon::tick_cache::set_config_reuse_max_age_override(self.prior);
    }
}

/// RAII pin of the daemon tick cache's module-reuse ceiling, restoring the
/// prior setting on drop. [`ConfigReuseMaxAgeGuard`]'s counterpart for the
/// resolved module set, which stands for the same thirty seconds the git
/// refresh window and the enumeration memo carry.
///
/// Kept separate from its sibling rather than folded into one guard, for the
/// same reason the five memo ceilings each keep their own constant: they answer
/// different questions, and one pin that moved both would make a test unable to
/// say which ceiling it was asserting about. Pair every use with
/// `#[serial_test::serial(tick_cache_reuse)]`.
pub struct ModuleReuseTtlGuard {
    prior: Option<u64>,
}

impl ModuleReuseTtlGuard {
    /// Pin the ceiling to `ttl`, saturating at the millisecond range.
    pub fn pinned(ttl: std::time::Duration) -> Self {
        let millis = u64::try_from(ttl.as_millis())
            .unwrap_or(u64::MAX)
            .min(u64::MAX - 1);
        Self {
            prior: crate::daemon::tick_cache::set_module_reuse_ttl_override(Some(millis)),
        }
    }

    /// Pin the ceiling beyond any test's lifetime.
    pub fn never_expires() -> Self {
        Self::pinned(std::time::Duration::from_millis(u64::MAX - 1))
    }

    /// Pin the ceiling to zero, so every resolution is stale the moment it is
    /// stored.
    pub fn always_expired() -> Self {
        Self::pinned(std::time::Duration::ZERO)
    }
}

impl Drop for ModuleReuseTtlGuard {
    fn drop(&mut self) {
        crate::daemon::tick_cache::set_module_reuse_ttl_override(self.prior);
    }
}

/// RAII pin of the module git-cache refresh window, restoring the prior setting
/// on drop. The third sibling of [`CommandPathMemoTtlGuard`], guarding the
/// window inside which one `git fetch` of a repository serves every later ask
/// for it.
///
/// Any test that drives TWO fetches of one fixture repository and expects the
/// second to really transfer — a tag pushed upstream between them — pins
/// `always_expired`, because the window is exactly what would otherwise serve
/// the first transfer's answer to the second ask. A test whose claim is that one
/// transfer served both pins `never_expires`, so the assertion is about the
/// mechanism rather than about how long two adjacent statements took.
///
/// **The pin serializes itself**: constructing a guard takes a process-global
/// mutex that is held until it drops, so two pins cannot overlap however the
/// tests holding them are scheduled. That is not the sibling guards' contract
/// and it is deliberate. Per-test temp directories keep two tests from sharing a
/// key in the refresh MAP, but the pin is a single process-global atomic, and
/// every test that reads this window asserts on whether a transfer HAPPENED —
/// precisely what the pin decides. A concurrent `always_expired` landing inside
/// a `never_expires` test opens the window to zero, its second fetch reaches for
/// an upstream the fixture has deleted, and the test goes hard red.
/// [`AvailableVersionMemoTtlGuard`] closes the same hazard with a named
/// `serial_test` group; this one cannot, because its users ALSO need the
/// unnamed group for `CFGD_ALLOW_LOCAL_SOURCES` and `serial_test` accepts only
/// ident keys, so "the default group AND a named one" is inexpressible. Building
/// the exclusion into the guard needs no attribute at the call site, so no
/// PINNING test can forget it.
///
/// What the exclusion does NOT cover: a test that drives a fetch without pinning
/// at all. The pin is one process-global atomic and an unpinned test reads
/// whatever is currently pinned, so a `never_expires` held here can spare an
/// unpinned concurrent test a transfer it was counting on. Every test whose
/// claim turns on whether a transfer happened must therefore pin, whichever
/// direction it needs — the guard makes the exclusion automatic among pins, not
/// among readers.
pub struct GitRefreshWindowGuard {
    prior: Option<u64>,
    // Ordered after `prior` only for readability; `Drop` restores the atomic
    // explicitly and the lock is released after that, when this field drops.
    _lock: std::sync::MutexGuard<'static, ()>,
}

/// Held for the life of every [`GitRefreshWindowGuard`], so at most one pin of
/// the refresh window exists in the process at a time.
static GIT_REFRESH_WINDOW_PIN: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl GitRefreshWindowGuard {
    /// Pin the window to `ttl`, saturating at the millisecond range. Blocks
    /// while another guard is alive.
    pub fn pinned(ttl: std::time::Duration) -> Self {
        // A test that panicked while pinned poisoned the mutex; the pin it left
        // behind was already restored by its own `Drop` during the unwind, so
        // the lock still hands out sound exclusion.
        let lock = GIT_REFRESH_WINDOW_PIN
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // `u64::MAX` is the "no override" sentinel, so a pin that would land on
        // it saturates one below: `pinned(Duration::MAX)` must pin the window
        // out of reach, never silently restore the default it was called to
        // displace.
        let millis = u64::try_from(ttl.as_millis())
            .unwrap_or(u64::MAX)
            .min(u64::MAX - 1);
        Self {
            prior: crate::modules::set_repo_refresh_ttl_override(Some(millis)),
            _lock: lock,
        }
    }

    /// Pin the window beyond any test's lifetime, so no recorded transfer can
    /// age out mid-test. For a test whose claim is that one transfer stands.
    pub fn never_expires() -> Self {
        Self::pinned(std::time::Duration::from_millis(u64::MAX - 1))
    }

    /// Pin the window to zero, so every repository is fetched on every ask. For
    /// a test whose claim is about what a transfer itself does.
    pub fn always_expired() -> Self {
        Self::pinned(std::time::Duration::ZERO)
    }
}

impl Drop for GitRefreshWindowGuard {
    fn drop(&mut self) {
        crate::modules::set_repo_refresh_ttl_override(self.prior);
    }
}

// ---------------------------------------------------------------------------
// Env-var test guards — replace per-file `struct EnvVarGuard` / `fn with_env`
// duplicates. Pair with `serial_test::serial` because env-var mutation is
// process-global.
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
        refuse_unbracketed_path_write(key);
        // SAFETY: serial_test::serial gates execution; no concurrent reader/writer.
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, prior }
    }

    /// Capture the prior value of `key`, then remove it.
    pub fn unset(key: &'static str) -> Self {
        let prior = std::env::var(key).ok();
        refuse_unbracketed_path_write(key);
        // SAFETY: serial_test::serial gates execution; no concurrent reader/writer.
        unsafe {
            std::env::remove_var(key);
        }
        Self { key, prior }
    }
}

/// `PATH` is read by every `command_path` resolution and by every spawn, so a
/// write to it is only sound inside the window [`path_env_mutation_guard`]
/// holds: the guard is what blocks a concurrent reader, and declaring it AFTER
/// the `EnvVarGuard` leaves the restore outside the window it was supposed to
/// bracket. Debug-only, so the guard's own restore path pays nothing in
/// release, and it is a deterministic tell rather than a convention.
fn refuse_unbracketed_path_write(key: &str) {
    debug_assert!(
        key != "PATH" || path_env_exclusive_guard_held(),
        "a PATH write must sit inside path_env_mutation_guard()'s window; \
         declare the mutation guard BEFORE the EnvVarGuard so it drops last"
    );
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        refuse_unbracketed_path_write(self.key);
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

/// RAII guard that saves the current working directory on construction,
/// changes to a new directory, and restores the prior directory on drop —
/// even if a test panics between construction and drop.
///
/// Holds the exclusive spawn-environment guard for its whole lifetime (see
/// [`path_env_mutation_guard`]): the working directory is inherited by every
/// child process and read by every relative-path helper, so a parallel test
/// must not spawn — or resolve `.git` from `.` — inside the window. That makes
/// the guard, not `#[serial]`, the thing that actually excludes the racing
/// majority.
///
/// Use this instead of paired `std::env::set_current_dir(&orig)` calls in
/// tests that need to drive CWD-sensitive helpers (e.g. git rev-parse,
/// path resolution from "."). The paired form leaks a dangling CWD when
/// an assertion between the two calls panics, and can capture *another*
/// test's temp directory as its "original", restoring the process to a
/// directory that no longer exists.
pub struct CwdGuard {
    orig: PathBuf,
    _spawn_excl: ExclusiveEnvGuard,
}

impl CwdGuard {
    /// Capture the current working directory, then change to `new`.
    /// Returns an error if either step fails.
    // env-mutator-ok: sets the process working directory, which is not an environment variable.
    pub fn set(new: impl AsRef<Path>) -> std::io::Result<Self> {
        let spawn_excl = path_env_mutation_guard();
        let orig = std::env::current_dir()?;
        std::env::set_current_dir(new)?;
        Ok(Self {
            orig,
            _spawn_excl: spawn_excl,
        })
    }
}

impl Drop for CwdGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.orig);
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
// is process-global.
//
// Cross-platform: points `CFGD_COSIGN_BIN` at the compiled `fake-cosign`
// binary (`src/bin/fake_cosign.rs`), built for the host target, instead of a
// `/bin/sh` script. Windows, macOS, and Linux run identical cosign coverage —
// no shell required. Per-invocation behavior flows to that binary through env
// vars (see the table below) so a single artifact serves every variant.
// ---------------------------------------------------------------------------
//
// Env vars owned by this shim (all captured on `install()` and restored on
// drop, even if the test panics):
// - CFGD_COSIGN_BIN           — points cosign_cmd() at the fake-cosign binary
// - CFGD_FAKE_COSIGN_LOG      — argv log path; set only when argv logging is on
// - CFGD_FAKE_COSIGN_KEYGEN   — "1" enables `generate-key-pair` key-file emit
// - CFGD_FAKE_COSIGN_STDERR   — stderr the fake emits on every invocation
// - CFGD_FAKE_COSIGN_EXIT     — the fake's exit code

/// Resolve the compiled `fake-cosign` binary from the running test binary.
///
/// Cargo/nextest place a crate's `[[bin]]` artifacts in the target profile
/// dir (`target/<profile>/fake-cosign[.exe]`), while the test binary itself
/// runs from the sibling `deps/` subdir (`target/<profile>/deps/<test>`). So
/// the fake sits one directory up from `current_exe()`'s parent, with the
/// platform executable suffix appended. Paths stay as `PathBuf`/`OsStr`
/// end-to-end — never rendered to a `String` — so the value handed to
/// `Command::new` keeps its native form for the host OS (correct for process
/// spawning on Windows; no separator folding applies to a spawn target).
#[cfg(any(test, feature = "test-helpers"))]
fn fake_cosign_bin_path() -> std::path::PathBuf {
    let test_exe = std::env::current_exe().expect("current_exe for fake-cosign lookup");
    let profile_dir = test_exe
        .parent() // .../target/<profile>/deps
        .and_then(std::path::Path::parent) // .../target/<profile>
        .expect("test binary must live under target/<profile>/deps");
    let mut bin = profile_dir.join("fake-cosign");
    bin.set_extension(std::env::consts::EXE_EXTENSION);
    assert!(
        bin.exists(),
        "fake-cosign binary not found at {} — ensure the cfgd-core `fake-cosign` \
         [[bin]] target is built (cargo nextest/test compiles it automatically)",
        crate::PathDisplayExt::posix(&bin),
    );
    bin
}

/// Builder + RAII guard for a fake `cosign` binary. Configure with the
/// `with_*` methods, then call [`CosignTestShim::install`] to point
/// `CFGD_COSIGN_BIN` at the compiled fake. Drops the env vars when the
/// returned value goes out of scope.
///
/// Cross-platform: the fake is the compiled `fake-cosign` binary, so consumers
/// run identically on Windows, macOS, and Linux — no `#[cfg(unix)]` gate
/// required.
pub struct CosignTestShim {
    log_path: Option<std::path::PathBuf>,
    argv_logging: bool,
    _tmp: tempfile::TempDir,
    prior: CosignEnvSnapshot,
}

/// Prior values of every env var the shim mutates, captured on install and
/// restored on drop so nested/sequential shims leave the process env clean.
struct CosignEnvSnapshot {
    bin: Option<std::ffi::OsString>,
    log: Option<std::ffi::OsString>,
    keygen: Option<std::ffi::OsString>,
    stderr: Option<std::ffi::OsString>,
    exit: Option<std::ffi::OsString>,
}

impl CosignTestShim {
    /// Builder entry point. Chain `with_*` methods then call [`CosignTestShimBuilder::install`].
    pub fn builder() -> CosignTestShimBuilder {
        CosignTestShimBuilder {
            argv_logging: true,
            keygen: false,
            exit_code: 0,
            stderr: String::new(),
        }
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
        match (&self.argv_logging, &self.log_path) {
            // absent-file-ok: a shim nothing ran wrote no log.
            (true, Some(path)) => std::fs::read_to_string(path).unwrap_or_default(),
            _ => String::new(),
        }
    }

    /// Number of times the shim was invoked. Returns 0 if argv logging is
    /// disabled.
    pub fn invocation_count(&self) -> usize {
        self.argv_log().lines().filter(|l| !l.is_empty()).count()
    }
}

impl Drop for CosignTestShim {
    fn drop(&mut self) {
        // SAFETY: callers wrap with `serial_test::serial`, so no concurrent
        // reader observes a mid-update env state.
        unsafe {
            restore_env("CFGD_COSIGN_BIN", self.prior.bin.take());
            restore_env("CFGD_FAKE_COSIGN_LOG", self.prior.log.take());
            restore_env("CFGD_FAKE_COSIGN_KEYGEN", self.prior.keygen.take());
            restore_env("CFGD_FAKE_COSIGN_STDERR", self.prior.stderr.take());
            restore_env("CFGD_FAKE_COSIGN_EXIT", self.prior.exit.take());
        }
    }
}

/// Set `var` to `prior` if it was present, else remove it.
///
/// # Safety
/// The process must not be reading the environment concurrently; the shim's
/// `serial_test::serial` requirement guarantees this.
unsafe fn restore_env(var: &str, prior: Option<std::ffi::OsString>) {
    unsafe {
        match prior {
            Some(v) => std::env::set_var(var, v),
            None => std::env::remove_var(var),
        }
    }
}

/// Builder for [`CosignTestShim`], reached only through
/// [`CosignTestShim::builder`], which seeds the most common existing variant:
/// argv logging on, keygen off, exit 0, empty stderr.
///
/// The fields are private and the type carries no `Default`, so that
/// constructor is the only way to hold one — which is what lets the roster
/// count `CosignTestShim::builder` and watch every test that reaches this
/// builder's own environment write.
pub struct CosignTestShimBuilder {
    argv_logging: bool,
    keygen: bool,
    exit_code: i32,
    stderr: String,
}

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

    /// Point `CFGD_COSIGN_BIN` at the compiled fake-cosign binary and set the
    /// per-invocation behavior env vars (`CFGD_FAKE_COSIGN_{LOG,KEYGEN,STDERR,
    /// EXIT}`). Prior values of every mutated var are captured for restoration
    /// on drop. A tempdir holds the argv log; it is removed with the guard.
    // env-mutator-ok: a builder exists only through `CosignTestShim::builder`, which the roster counts.
    pub fn install(self) -> CosignTestShim {
        let bin_path = fake_cosign_bin_path();
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let log_path = tmp.path().join("argv.log");

        // Capture prior values of every var the shim mutates.
        let prior = CosignEnvSnapshot {
            bin: std::env::var_os("CFGD_COSIGN_BIN"),
            log: std::env::var_os("CFGD_FAKE_COSIGN_LOG"),
            keygen: std::env::var_os("CFGD_FAKE_COSIGN_KEYGEN"),
            stderr: std::env::var_os("CFGD_FAKE_COSIGN_STDERR"),
            exit: std::env::var_os("CFGD_FAKE_COSIGN_EXIT"),
        };

        // SAFETY: callers wrap with `serial_test::serial`, so no concurrent
        // reader observes a mid-update env state. Path values stay as
        // `Path`/`OsStr` so `Command::new` receives the host-native form.
        unsafe {
            std::env::set_var("CFGD_COSIGN_BIN", &bin_path);
            if self.argv_logging {
                std::env::set_var("CFGD_FAKE_COSIGN_LOG", &log_path);
            } else {
                std::env::remove_var("CFGD_FAKE_COSIGN_LOG");
            }
            if self.keygen {
                std::env::set_var("CFGD_FAKE_COSIGN_KEYGEN", "1");
            } else {
                std::env::remove_var("CFGD_FAKE_COSIGN_KEYGEN");
            }
            std::env::set_var("CFGD_FAKE_COSIGN_STDERR", &self.stderr);
            std::env::set_var("CFGD_FAKE_COSIGN_EXIT", self.exit_code.to_string());
        }

        CosignTestShim {
            log_path: self.argv_logging.then_some(log_path),
            argv_logging: self.argv_logging,
            _tmp: tmp,
            prior,
        }
    }
}

// ---------------------------------------------------------------------------
// MockPackageManager — reusable mock for reconciler and module tests.
// Consolidates the per-file `FakePkgMgr` definitions into one shared mock
// with configurable installed set and install-call recording.
// ---------------------------------------------------------------------------

/// A mock `PackageManager` that tracks install/uninstall calls and reports
/// a configurable set of installed packages.
pub struct MockPackageManager {
    pub mgr_name: String,
    pub available: bool,
    pub bootstrap_capable: bool,
    pub bootstrap_method: String,
    pub bootstrap_requires: Vec<String>,
    pub bootstrap_creates: Vec<String>,
    /// The mediators this mock's cascade would install through, in order,
    /// taken when the plan says the run delivers one; `bootstrap_method` is
    /// the host arm it falls to otherwise. The npm/pipx/go shape.
    pub bootstrap_cascade: Vec<String>,
    /// What `tool_version()` answers, for a test about the detail a landed
    /// provision states.
    pub tool_version: Option<String>,
    pub installed: std::collections::HashSet<String>,
    pub install_calls: Mutex<Vec<Vec<String>>>,
    pub uninstall_calls: Mutex<Vec<Vec<String>>>,
    /// Whether `bootstrap()` leaves this manager available, for a test that
    /// must drive a `Provision` node through to a real success rather than
    /// the default `BootstrapFailed` a fixed `available` flag always yields.
    pub bootstrap_succeeds: bool,
    /// Interior mutability because `PackageManager::bootstrap` takes `&self` —
    /// `is_available()` is read again immediately after, and `available`
    /// itself is a plain `bool` a `&self` call cannot flip.
    became_available: std::sync::atomic::AtomicBool,
    /// Sleep this long inside `install()` before returning — holds a lane open
    /// long enough for the others to be seen in it.
    install_delay: Option<std::time::Duration>,
    /// Shared counter of installs in flight, for a test proving lanes overlap.
    witness: Option<std::sync::Arc<ConcurrencyWitness>>,
    /// Whether this manager keeps a local index. `true` by default because
    /// most fixtures want the refresh node in the tree; `without_index()` is
    /// the `cargo`/`npm` shape, which must plan none.
    keeps_index: bool,
    /// What a mediator installs to deliver this manager, keyed by mediator
    /// name — the answer `PackageManager::mediated_packages` gives, and so
    /// what decides whether the planner may batch this manager's provision
    /// onto a sibling's. Empty by default: a mock is unbatchable until a
    /// fixture says otherwise.
    mediated: std::collections::BTreeMap<String, Vec<String>>,
    /// When set, `is_available()` reads this flag instead of `available`.
    /// That is the shape a provisioned manager really has: npm appears on the
    /// host when APT's install lands, not when npm's own bootstrap is called —
    /// and under a batched provision no member's `bootstrap` runs at all, so a
    /// mock that can only flip itself can never reach the post-install
    /// availability check.
    availability: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Raised by this manager's `install()`. The mediator half of the pair
    /// above.
    raises: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Every `install()` this manager was asked to run, shared with the test
    /// that owns the registry — a `Box<dyn PackageManager>` cannot be read
    /// back for its own `install_calls`.
    install_log: Option<std::sync::Arc<Mutex<Vec<Vec<String>>>>>,
    /// How many times this manager was asked to enumerate what it has
    /// installed. The observable behind every "asked once per manager, not once
    /// per package" claim — a count, never a duration — and shared, because a
    /// `Box<dyn PackageManager>` in a registry cannot be read back for it.
    enumerations: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Whether declared and listed names fold to lowercase, the way a
    /// case-insensitive manager's identity space does.
    folds_case: bool,
    /// Whether this mock's entries register package SOURCES for its family,
    /// the `brew-tap` shape — the flag every tap-first ordering surface reads.
    registers_sources: bool,
    /// What this manager's listing reports as each package's installed
    /// version, keyed by name. A name with no entry lists as
    /// `UNKNOWN_PACKAGE_VERSION`, the trait default's answer.
    versions: std::collections::BTreeMap<String, String>,
    /// Everything `install()` has landed, folded into `installed_packages`
    /// alongside `installed`.
    ///
    /// A real manager reports a package it just installed, and a mock that did
    /// not was the only reason a run could install one package twice without a
    /// test noticing: the `Bootstrap` phase provisions `npm` with `apt
    /// install npm`, and the `Packages` phase then asked apt for `npm` again.
    landed: std::sync::Arc<Mutex<std::collections::HashSet<String>>>,
    /// Whether `version_meets_minimum_checked` fails instead of answering —
    /// the FreeBSD `pkg version -t` shape, whose comparator genuinely shells
    /// out and can fail to spawn.
    comparisons_fail: bool,
    /// Whether `upgrade_verb()` answers `None` — a manager that cannot raise
    /// a package in place at all, so a below-floor package is a check error
    /// rather than a planned raise. `false` by default: every manager that
    /// lists versions carries a raise verb, distinct or its own install.
    no_upgrade_verb: bool,
}

impl MockPackageManager {
    pub fn new(name: &str) -> Self {
        Self {
            mgr_name: name.to_string(),
            available: true,
            bootstrap_capable: false,
            bootstrap_method: "mock".to_string(),
            bootstrap_cascade: Vec::new(),
            tool_version: None,
            bootstrap_requires: Vec::new(),
            bootstrap_creates: Vec::new(),
            installed: std::collections::HashSet::new(),
            install_calls: Mutex::new(Vec::new()),
            uninstall_calls: Mutex::new(Vec::new()),
            bootstrap_succeeds: false,
            became_available: std::sync::atomic::AtomicBool::new(false),
            install_delay: None,
            witness: None,
            keeps_index: true,
            mediated: std::collections::BTreeMap::new(),
            availability: None,
            raises: None,
            landed: std::sync::Arc::default(),
            install_log: None,
            enumerations: std::sync::Arc::default(),
            folds_case: false,
            registers_sources: false,
            versions: std::collections::BTreeMap::new(),
            comparisons_fail: false,
            no_upgrade_verb: false,
        }
    }

    /// The `pkg version -t` shape: the version comparator fails to spawn
    /// rather than answering, so a caller judging a floor gets a check error
    /// instead of a `bool`.
    pub fn failing_version_comparisons(mut self) -> Self {
        self.comparisons_fail = true;
        self
    }

    /// A manager that cannot raise an already-held package in place at all —
    /// `upgrade_verb()` answers `None`, so a below-floor package becomes a
    /// check error rather than a planned raise.
    pub fn without_upgrade_verb(mut self) -> Self {
        self.no_upgrade_verb = true;
        self
    }

    /// The `brew-tap` shape: entries are package SOURCES for the family, so
    /// ordering surfaces put this mock's installs first.
    pub fn registering_family_sources(mut self) -> Self {
        self.registers_sources = true;
        self
    }

    /// The chocolatey/scoop/winget shape: the identity space is lowercase, so
    /// a module declaring `Wget` must match a listing of `wget`.
    pub fn case_insensitive(mut self) -> Self {
        self.folds_case = true;
        self
    }

    /// The shared counter of this manager's enumerations, taken BEFORE the
    /// manager is boxed into a registry.
    pub fn enumeration_counter(&self) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
        std::sync::Arc::clone(&self.enumerations)
    }

    /// The `cargo`/`npm`/`pipx` shape: no local index, so no refresh node.
    pub fn without_index(mut self) -> Self {
        self.keeps_index = false;
        self
    }

    pub fn with_installed(mut self, pkgs: &[&str]) -> Self {
        for p in pkgs {
            self.installed.insert((*p).to_string());
        }
        self
    }

    /// Installed, and reporting `version` for it — the shape a manager whose
    /// listing states versions really has, so a declared `minVersion` floor
    /// has something to be judged against. Spelled the same as
    /// `StubPackageManager::with_installed_at` in cfgd-core's own tests, since
    /// the two mocks answer the same question. A name left out keeps
    /// `UNKNOWN_PACKAGE_VERSION`, which is what a manager that cannot state a
    /// version answers.
    pub fn with_installed_at(mut self, pkg: &str, version: &str) -> Self {
        self.installed.insert(pkg.to_string());
        self.versions.insert(pkg.to_string(), version.to_string());
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

    /// Bootstrappable, with the method its plan names.
    /// A cascade that prefers `mediators` (in order) whenever the run delivers
    /// one, and otherwise takes `bootstrappable_via`'s method.
    pub fn preferring_delivered(mut self, mediators: &[&str]) -> Self {
        self.bootstrap_capable = true;
        self.bootstrap_cascade = mediators.iter().map(|m| (*m).to_string()).collect();
        self
    }

    pub fn bootstrappable_via(mut self, method: &str) -> Self {
        self.bootstrap_capable = true;
        self.bootstrap_method = method.to_string();
        self
    }

    /// Name the tools this manager's bootstrap plan shells out to — the
    /// population the `Bootstrap` phase draws a prerequisite node from.
    pub fn requiring(mut self, tools: &[&str]) -> Self {
        self.bootstrap_requires = tools.iter().map(|t| (*t).to_string()).collect();
        self
    }

    /// Name the PATH directories this manager's bootstrap plan declares —
    /// the population `fold_provision_path_dirs` folds into the planned Env
    /// content for a manager this run will provision.
    pub fn creating_dirs(mut self, dirs: &[&str]) -> Self {
        self.bootstrap_creates = dirs.iter().map(|d| (*d).to_string()).collect();
        self
    }

    /// Report this version once available, so a landed provision has a fact
    /// to state beside its row.
    pub fn reporting_version(mut self, version: &str) -> Self {
        self.tool_version = Some(version.to_string());
        self
    }

    /// Make `bootstrap()` leave this manager available, so a `Provision` node
    /// driven through a real `apply()` settles as a success instead of the
    /// `BootstrapFailed` a manager stuck `unavailable()` always yields.
    pub fn bootstrap_succeeds(mut self) -> Self {
        self.bootstrap_succeeds = true;
        self
    }

    /// Make `install()` sleep for `delay` before returning — for a test that
    /// proves the lane dispatcher runs independent managers concurrently
    /// rather than one after another.
    pub fn with_install_delay(mut self, delay: std::time::Duration) -> Self {
        self.install_delay = Some(delay);
        self
    }

    /// Declare that `via` delivers this manager by installing `packages` —
    /// the `npm`-from-`apt` shape, and the only thing that makes a provision
    /// batchable.
    pub fn mediated_by(mut self, via: &str, packages: &[&str]) -> Self {
        self.mediated.insert(
            via.to_string(),
            packages.iter().map(|p| (*p).to_string()).collect(),
        );
        self
    }

    /// Read availability from `flag` rather than from a fixed bool.
    pub fn available_when(mut self, flag: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Self {
        self.availability = Some(flag);
        self
    }

    /// Raise `flag` from this manager's `install()` — the mediator that
    /// delivers whatever reads the same flag through `available_when`.
    pub fn raising(mut self, flag: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Self {
        self.raises = Some(flag);
        self
    }

    /// Record every `install()` into a log the test keeps a handle on.
    pub fn recording_installs(mut self, log: std::sync::Arc<Mutex<Vec<Vec<String>>>>) -> Self {
        self.install_log = Some(log);
        self
    }

    /// Report this manager's installs to a witness shared with its peers.
    pub fn with_concurrency_witness(mut self, witness: std::sync::Arc<ConcurrencyWitness>) -> Self {
        self.witness = Some(witness);
        self
    }
}

/// How many mock installs were ever in flight at the same moment.
///
/// The direct observation of the thing a concurrency test is about. A wall
/// clock cannot make that claim: "faster than a serial walk would be" is a
/// margin, and a loaded test binary spends it on scheduling rather than on
/// work — the same run reads 200ms alone and 400ms beside 800 other tests, so
/// the bound is either too tight to be reliable or too loose to falsify
/// anything. A peak of one IS a serial walk, whatever the clock said.
#[derive(Default)]
pub struct ConcurrencyWitness {
    live: std::sync::atomic::AtomicUsize,
    peak: std::sync::atomic::AtomicUsize,
}

impl ConcurrencyWitness {
    pub fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self::default())
    }

    /// The most installs seen running at once.
    pub fn peak(&self) -> usize {
        self.peak.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn enter(&self) -> WitnessGuard<'_> {
        let live = self.live.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        self.peak
            .fetch_max(live, std::sync::atomic::Ordering::SeqCst);
        WitnessGuard { witness: self }
    }
}

/// Leaves the count where it found it, including on a panicking install.
struct WitnessGuard<'a> {
    witness: &'a ConcurrencyWitness,
}

impl Drop for WitnessGuard<'_> {
    fn drop(&mut self) {
        self.witness
            .live
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl crate::providers::PackageManager for MockPackageManager {
    fn name(&self) -> &str {
        &self.mgr_name
    }

    fn upgrade_verb(&self) -> Option<&'static str> {
        (!self.no_upgrade_verb).then_some("upgrade")
    }

    fn is_available(&self) -> bool {
        if let Some(flag) = &self.availability {
            return flag.load(std::sync::atomic::Ordering::SeqCst);
        }
        self.available
            || self
                .became_available
                .load(std::sync::atomic::Ordering::SeqCst)
    }

    fn bootstrap_plan_given(
        &self,
        delivered: &dyn Fn(&str) -> bool,
    ) -> Option<crate::providers::BootstrapPlan> {
        self.bootstrap_capable.then(|| {
            let method = self
                .bootstrap_cascade
                .iter()
                .find(|m| delivered(m))
                .unwrap_or(&self.bootstrap_method);
            crate::providers::BootstrapPlan::new(method.clone())
                .requiring(self.bootstrap_requires.clone())
                .creating(self.bootstrap_creates.clone())
        })
    }

    fn tool_version(&self) -> Option<String> {
        self.tool_version.clone()
    }

    fn bootstrap(&self, _cx: &crate::providers::PackageContext<'_>) -> crate::errors::Result<()> {
        if self.bootstrap_succeeds {
            self.became_available
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        Ok(())
    }

    fn mediated_packages(&self, via: &str) -> Option<Vec<String>> {
        self.mediated.get(via).cloned()
    }

    fn path_dirs(&self, _cx: &crate::providers::PackageContext<'_>) -> Vec<String> {
        self.bootstrap_creates.clone()
    }

    fn installed_packages(
        &self,
        _cx: &crate::providers::PackageContext<'_>,
    ) -> crate::errors::Result<std::collections::HashSet<String>> {
        self.enumerations
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut reported = self.installed.clone();
        reported.extend(self.landed.lock().unwrap().iter().cloned());
        Ok(reported)
    }

    fn installed_packages_with_versions(
        &self,
        cx: &crate::providers::PackageContext<'_>,
    ) -> crate::errors::Result<Vec<crate::providers::PackageInfo>> {
        Ok(self
            .installed_packages(cx)?
            .into_iter()
            .map(|name| {
                let version = self
                    .versions
                    .get(&name)
                    .cloned()
                    .unwrap_or_else(|| crate::providers::UNKNOWN_PACKAGE_VERSION.to_string());
                crate::providers::PackageInfo { name, version }
            })
            .collect())
    }

    fn package_identity(&self, entry: &str) -> String {
        if self.folds_case {
            entry.to_lowercase()
        } else {
            entry.to_string()
        }
    }

    fn listed_identity(&self, listed_name: &str) -> String {
        if self.folds_case {
            listed_name.to_lowercase()
        } else {
            listed_name.to_string()
        }
    }

    fn install(
        &self,
        packages: &[String],
        _cx: &crate::providers::PackageContext<'_>,
    ) -> crate::errors::Result<()> {
        let _in_flight = self.witness.as_ref().map(|w| w.enter());
        if let Some(delay) = self.install_delay {
            // sleep-ok: simulates a slow install to widen the overlap window a ConcurrencyWitness observes — the witness peak is the actual assertion, not this duration
            std::thread::sleep(delay);
        }
        self.install_calls.lock().unwrap().push(packages.to_vec());
        self.landed.lock().unwrap().extend(packages.iter().cloned());
        if let Some(log) = &self.install_log {
            log.lock().unwrap().push(packages.to_vec());
        }
        if let Some(flag) = &self.raises {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        Ok(())
    }

    fn uninstall(
        &self,
        packages: &[String],
        _cx: &crate::providers::PackageContext<'_>,
    ) -> crate::errors::Result<()> {
        self.uninstall_calls.lock().unwrap().push(packages.to_vec());
        Ok(())
    }

    fn has_index(&self) -> bool {
        self.keeps_index
    }

    fn registers_family_sources(&self) -> bool {
        self.registers_sources
    }

    fn refresh_index(
        &self,
        _cx: &crate::providers::PackageContext<'_>,
    ) -> crate::errors::Result<()> {
        Ok(())
    }

    fn available_version(&self, _package: &str) -> crate::errors::Result<Option<String>> {
        Ok(None)
    }

    fn version_meets_minimum_checked(
        &self,
        available: &str,
        min_version: &str,
    ) -> std::result::Result<bool, String> {
        if self.comparisons_fail {
            return Err("mock comparator failed to spawn".to_string());
        }
        Ok(self.version_meets_minimum(available, min_version))
    }
}

// ---------------------------------------------------------------------------
// ReconcilerTestHarness — builder that wires the full reconciler stack in ~5
// lines per test, replacing ~40 lines of manual ProviderRegistry construction.
// ---------------------------------------------------------------------------

/// Builder for [`ReconcilerTestHarness`].
pub struct ReconcilerTestHarnessBuilder {
    profile_yaml: Option<String>,
    package_managers: Vec<MockPackageManager>,
    system_configurators: Vec<MockSystemConfigurator>,
    secret_providers: Vec<MockSecretProvider>,
    file_manager: Option<MockFileManager>,
}

impl ReconcilerTestHarnessBuilder {
    /// Set the profile YAML that will be parsed into a `ResolvedProfile`.
    /// If not called, `make_empty_resolved()` is used.
    pub fn profile_yaml(mut self, yaml: &str) -> Self {
        self.profile_yaml = Some(yaml.to_string());
        self
    }

    /// Add a mock package manager with the given name and set of installed packages.
    pub fn package_manager(mut self, name: &str, installed: &[&str]) -> Self {
        self.package_managers
            .push(MockPackageManager::new(name).with_installed(installed));
        self
    }

    /// Add an already-built mock package manager, for a test that configures
    /// availability or a bootstrap plan the `(name, installed)` shorthand
    /// cannot express.
    pub fn with_package_manager(mut self, pm: MockPackageManager) -> Self {
        self.package_managers.push(pm);
        self
    }

    /// Add a mock system configurator with no drift.
    pub fn system_configurator(mut self, name: &str, _drift: &[SystemDrift]) -> Self {
        self.system_configurators
            .push(MockSystemConfigurator::new(name));
        self
    }

    /// Add a mock system configurator with pre-configured drift entries.
    pub fn system_configurator_with_drift(mut self, name: &str, drift: Vec<SystemDrift>) -> Self {
        self.system_configurators
            .push(MockSystemConfigurator::new(name).with_drift(drift));
        self
    }

    /// Add a mock secret provider that resolves to the given value.
    pub fn secret_provider(mut self, name: &str, resolved_value: &str) -> Self {
        self.secret_providers
            .push(MockSecretProvider::new(name).with_resolve_result(resolved_value));
        self
    }

    /// Set a custom mock file manager. If not called, a default `MockFileManager` is used.
    pub fn file_manager(mut self, fm: MockFileManager) -> Self {
        self.file_manager = Some(fm);
        self
    }

    /// Build the harness, wiring all mocks into a `ProviderRegistry` and `StateStore`.
    pub fn build(self) -> ReconcilerTestHarness {
        let state = test_state();

        let resolved = if let Some(yaml) = &self.profile_yaml {
            parse_profile_yaml_to_resolved(yaml)
        } else {
            make_empty_resolved()
        };

        let mut registry = crate::providers::ProviderRegistry::new();

        for pm in self.package_managers {
            registry.add_package_manager(Box::new(pm));
        }
        for sc in self.system_configurators {
            registry.add_system_configurator(Box::new(sc));
        }
        for sp in self.secret_providers {
            registry.secret_providers.push(Box::new(sp));
        }

        let fm = self.file_manager.unwrap_or_default();
        registry.file_manager = Some(Box::new(fm));

        ReconcilerTestHarness {
            registry,
            state,
            resolved,
        }
    }
}

/// A fully-wired reconciler test stack. Owns the `ProviderRegistry`,
/// `StateStore`, and `ResolvedProfile` so tests can call `plan()` and
/// `apply()` with minimal ceremony.
pub struct ReconcilerTestHarness {
    pub registry: crate::providers::ProviderRegistry,
    pub state: crate::state::StateStore,
    pub resolved: crate::config::ResolvedProfile,
}

impl ReconcilerTestHarness {
    /// Entry point: returns a builder.
    pub fn builder() -> ReconcilerTestHarnessBuilder {
        ReconcilerTestHarnessBuilder {
            profile_yaml: None,
            package_managers: Vec::new(),
            system_configurators: Vec::new(),
            secret_providers: Vec::new(),
            file_manager: None,
        }
    }

    /// Generate a reconciliation plan with default (empty) actions and Apply context.
    pub fn plan(&self) -> crate::errors::Result<crate::reconciler::Plan> {
        let reconciler = crate::reconciler::Reconciler::new(&self.registry, &self.state);
        reconciler.plan(
            &self.resolved,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            crate::reconciler::ReconcileContext::Apply,
        )
    }

    /// Generate a plan with explicit package and file actions.
    pub fn plan_with_actions(
        &self,
        file_actions: Vec<crate::providers::FileAction>,
        pkg_actions: Vec<crate::providers::PackageAction>,
        module_actions: Vec<crate::modules::ResolvedModule>,
    ) -> crate::errors::Result<crate::reconciler::Plan> {
        let reconciler = crate::reconciler::Reconciler::new(&self.registry, &self.state);
        reconciler.plan(
            &self.resolved,
            file_actions,
            pkg_actions,
            module_actions,
            crate::reconciler::ReconcileContext::Apply,
        )
    }

    /// Apply a plan using the harness's registry and state. Uses a quiet printer.
    pub fn apply(
        &self,
        plan: &crate::reconciler::Plan,
        printer: &Printer,
    ) -> crate::errors::Result<crate::reconciler::ApplyResult> {
        self.apply_with_filter(plan, printer, None)
    }

    /// Apply a plan under an active `--phase`/`--skip` filter — the shape a
    /// test needs to reproduce "this run never reached the `Bootstrap`
    /// phase", since [`Self::apply`] always applies unfiltered.
    pub fn apply_with_filter(
        &self,
        plan: &crate::reconciler::Plan,
        printer: &Printer,
        phase_filter: Option<&crate::reconciler::PhaseFilter>,
    ) -> crate::errors::Result<crate::reconciler::ApplyResult> {
        let reconciler = crate::reconciler::Reconciler::new(&self.registry, &self.state);
        reconciler.apply(
            plan,
            &self.resolved,
            std::path::Path::new("."),
            printer,
            phase_filter,
            &[],
            crate::reconciler::ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
    }

    /// Borrow the `StateStore`.
    pub fn state_store(&self) -> &crate::state::StateStore {
        &self.state
    }

    /// Borrow the `ResolvedProfile`.
    pub fn resolved_profile(&self) -> &crate::config::ResolvedProfile {
        &self.resolved
    }
}

/// Parse a profile YAML string into a `ResolvedProfile` with a single local layer.
/// Accepts either a full `ProfileDocument` (with apiVersion/kind/metadata/spec) or
/// a bare `ProfileSpec`. Tries document form first, falls back to bare spec.
fn parse_profile_yaml_to_resolved(yaml: &str) -> crate::config::ResolvedProfile {
    let spec = if let Ok(doc) = serde_yaml::from_str::<crate::config::ProfileDocument>(yaml) {
        doc.spec
    } else {
        serde_yaml::from_str::<crate::config::ProfileSpec>(yaml)
            .expect("failed to parse profile YAML in test harness")
    };

    // Built through the production merge rather than field-by-field: the merge
    // is what records which layer declared each env var and alias, and a
    // harness that assembles the struct by hand hands the reconciler a profile
    // whose entries name no owner.
    let layers = vec![crate::config::ProfileLayer {
        source: crate::config::LOCAL_LAYER.to_string(),
        profile_name: "harness-test".to_string(),
        priority: 1000,
        policy: crate::config::LayerPolicy::Local,
        spec,
    }];
    let merged = crate::config::merge_layers(&layers);

    crate::config::ResolvedProfile { layers, merged }
}

/// Install a claude-code skill for `kind` at `scope`, then rewrite its stamped
/// `cfgd-version` to `0.0.1` so `list` flags it stale (stamp != running). The
/// whole-file claude provider carries the stamp on a `cfgd-version:` frontmatter
/// line, so a line rewrite faithfully reproduces an old install.
// env-mutator-ok: writes a skill file; its `install(…)` is `SkillProvider`'s, not a shim's.
pub fn seed_stale_skill(
    kind: crate::generate::SkillKind,
    scope: crate::providers::skill::SkillScope,
) -> std::path::PathBuf {
    use crate::providers::skill::{ClaudeCodeProvider, SkillProvider};

    let path = ClaudeCodeProvider
        .install(
            &crate::generate::skill_model_for(kind, env!("CARGO_PKG_VERSION")),
            scope,
        )
        .expect("install skill");
    let body = std::fs::read_to_string(&path).expect("read installed skill");
    let staled = body
        .lines()
        .map(|l| {
            if l.trim_start().starts_with("cfgd-version:") {
                "cfgd-version: 0.0.1".to_string()
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&path, staled).expect("rewrite stale stamp");
    path
}

/// Pin `store`'s recorded scan stamp at `timestamp` and refuse every later
/// write to it, so a caller can drive the refused-write branch of
/// [`crate::state::StateStore::record_scan`] and see what its own fallback
/// renders.
///
/// A file-level refusal rather than a connection-level one, because the
/// consumer of that refusal opens its OWN store from the same state directory
/// and nothing on this connection reaches it. The row stays READABLE — the
/// refusal is a pair of `RAISE(ABORT)` triggers, not a dropped table — since
/// the value a caller must show is the one already recorded.
///
/// Repeatable, and re-pinnable at a new stamp.
pub fn freeze_last_scan_at(
    store: &crate::state::StateStore,
    timestamp: &str,
) -> crate::errors::Result<()> {
    store.freeze_last_scan_at(timestamp)
}

/// A recorded backup payload gc cannot remove, held that way for as long as the
/// value lives.
///
/// The two operating systems refuse a removal for different reasons, so each
/// gets the shape its own kernel actually refuses. On unix the snapshot's
/// destination DIRECTORY is replaced with a file, so the recorded path runs
/// through a file and yields `NotADirectory`; Windows reports that same path as
/// `NotFound`, which the remover reads as a payload that was already gone, so
/// there the snapshot file itself is opened granting no sharing at all and every
/// `remove_file` against it — from this process or from a child running the real
/// binary — fails with a sharing violation. Dropping the value releases the
/// hold, so a caller binds it across the collection it is proving.
pub struct UnremovablePayload {
    /// What must still be on disk once gc has reported it could not remove the
    /// payload: the stand-in file on unix, the held snapshot on Windows. Both
    /// are files, so one question answers the claim on either OS.
    witness: PathBuf,
    #[cfg(windows)]
    _held_open: std::fs::File,
}

impl UnremovablePayload {
    /// Whether what was held unremovable is still there.
    pub fn witness_survives(&self) -> bool {
        self.witness.is_file()
    }
}

/// Make the recorded snapshot at `payload` unremovable; see
/// [`UnremovablePayload`] for the per-OS shape.
pub fn hold_payload_unremovable(payload: &Path) -> UnremovablePayload {
    #[cfg(unix)]
    {
        let dir = payload
            .parent()
            .expect("a recorded snapshot lives under a destination")
            .to_path_buf();
        std::fs::remove_dir_all(&dir).expect("clear the old destination");
        std::fs::write(&dir, b"an operator's file").expect("file in its place");
        UnremovablePayload { witness: dir }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(payload)
            .expect("hold the snapshot open");
        UnremovablePayload {
            witness: payload.to_path_buf(),
            _held_open: held,
        }
    }
}

/// The production region of a Rust source file a walk-style pin reads: the file
/// with EVERY top-level inline test module removed.
///
/// A pin that scans source has to drop the file's own `#[cfg(test)]` blocks,
/// which describe the surface rather than rendering it. What it must not drop is
/// production code, and both cheaper spellings do: cutting at the first bare
/// `#[cfg(test)]` blinds the walk below a mid-file `#[cfg(test)] use` import or
/// test-only `impl`, and cutting at the last `#[cfg(test)]\nmod ` blinds it below
/// an aggregator's one-line `mod tests;` DECLARATION, whose tests live in another
/// file (93% of `reconciler/mod.rs`, 59% of `daemon/mod.rs`). Both fail silently,
/// since a walk that reads nothing reports nothing.
///
/// So this is a strip, not a suffix cut: a trailing test module is only the
/// common case, and a file may hold several inline siblings (`output/mod.rs` has
/// six). A declaration survives — it carries no test text — and so does every
/// line between and after the blocks.
///
/// The shape relied on is rustfmt's, which every file in this workspace is
/// formatted by: the attribute alone at column 0, and the module's closing `}`
/// alone at column 0. Each anchor drops through the next such line. A platform
/// gate stacks a second attribute (`#[cfg(test)]` / `#[cfg(unix)]` /
/// `mod tests {`) between the marker and the `mod` line, so the scan skips
/// every column-0 attribute line before checking for `mod ` — three daemon
/// service files and `cli/kubectl.rs` carry exactly this shape, and without
/// the skip their whole test module read as production text. The one way the
/// rustfmt assumption breaks is a multi-line raw string inside a test module
/// whose body carries a bare `}` at column 0, which would end the strip early
/// — no such literal exists today, and `cli::tests::production_body` assumes
/// the same shape.
///
/// A walk over several files reads through [`production_slice_of`] instead,
/// which owns the read and the per-file floor that keeps a re-blinding from
/// passing quietly.
pub fn production_slice(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut lines = src.lines();
    while let Some(line) = lines.next() {
        if line == "#[cfg(test)]" {
            let mut rest = lines.clone();
            while rest.clone().next().is_some_and(|m| m.starts_with('#')) {
                rest.next();
            }
            if rest
                .clone()
                .next()
                .is_some_and(|m| m.starts_with("mod ") && m.trim_end().ends_with('{'))
            {
                rest.next();
                lines = rest;
                for inner in lines.by_ref() {
                    if inner == "}" {
                        break;
                    }
                }
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// The whole text of a file a walk ENUMERATED, read here so the read failure
/// cannot be separated from the population's floor.
///
/// A file a walk cannot open is otherwise indistinguishable from one holding
/// nothing: the walk judges it by no rule, reports no offender and passes having
/// read less than its floor promised. The WHOLE-file twin of
/// [`production_slice_of`], for a walk whose subject is a source's test region,
/// a golden or a markdown page rather than a source's production region. A read
/// whose absence is a legitimate state — an artifact the test itself decided not
/// to write — stays a silent read and says so with `// absent-file-ok: <why>`.
pub fn walked_file_body(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("{}: the walk must read every file: {e}", path.display()))
}

/// The production region of the Rust source at `path`, read here so the two
/// halves a multi-file walk needs cannot be separated: a source the walk cannot
/// read fails it, and a slice shorter than the lines preceding the file's first
/// `#[cfg(test)]` fails it, because [`production_slice`] drops a trailing test
/// module and nothing else, so a shorter read is a walk that went blind partway
/// down the file. A walk over several files reads every one through this;
/// [`production_slice`] stays the pure cut for a caller holding one body.
pub fn production_slice_of(path: &Path) -> String {
    let body = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("{}: the walk must read every source: {e}", path.display()));
    // unfloored-slice-ok: the floor over what this cut returned is the assert below.
    let production = production_slice(&body);
    let before_tests = body
        .lines()
        .position(|l| l == "#[cfg(test)]")
        .unwrap_or_else(|| body.lines().count());
    let walked = production.lines().count();
    assert!(
        walked > 0 && walked >= before_tests,
        "{}: the walk read {walked} lines of the {before_tests} that precede this file's test module",
        path.display()
    );
    production
}

/// The workspace root: the directory holding `crates/`.
///
/// `CARGO_MANIFEST_DIR` is resolved while THIS crate compiles, so it names
/// `<root>/crates/cfgd-core` whichever crate's test binary is running and a
/// consumer crate's walk reaches the same root as cfgd-core's own.
pub fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// Every file under `root`, at any depth, in whatever order the filesystem
/// lists them.
///
/// The failure policy every walk in this module shares: a directory it cannot
/// open, and an entry it cannot read, each fail the walk. Both are a walk gone
/// blind over whatever was there, and the population a caller then judges is
/// shorter than the one it claims to have read, which is a pass nobody asked
/// for.
fn files_under_root(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).unwrap_or_else(|e| {
            panic!("{}: the walk must read every directory: {e}", dir.display())
        });
        for entry in entries {
            let path = entry
                .unwrap_or_else(|e| {
                    panic!("{}: the walk must read every entry: {e}", dir.display())
                })
                .path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out
}

/// Every `.rs` source under `root`, sorted.
///
/// A directory the walk cannot open is a walk gone blind over whatever it held,
/// and a fence built on a short list passes by reading less than it claims, so
/// an unopenable directory and an empty result both fail here rather than
/// shrinking the population in silence. The order is the sort, so a walk's own
/// output and any message it builds read the same on every host.
pub fn rust_sources_under(root: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = files_under_root(root)
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "rs"))
        .collect();
    assert!(
        !out.is_empty(),
        "{}: the walk found no sources, so it proves nothing",
        root.display()
    );
    out.sort();
    out
}

/// Every path-based chmod in the production sources of every crate under
/// `crates_dir`, and the ones that do not say why following a symlink is safe.
///
/// One walk for the whole WORKSPACE, not one per crate: a walk reads source
/// TEXT, so the crate graph does not bound it, and a per-crate body is how three
/// crates (`cfgd-csi` above all, which runs as root on every node) ended up with
/// no walk at all. The roots are derived by reading `crates_dir` rather than
/// listed, so a crate added to the workspace joins the population with it, and
/// each root is a crate's `src`: a bare `crates/` root would read
/// `cfgd/tests/common/mod.rs` as production. A `build.rs` is outside the roots
/// and outside the class — it runs as the building user in its own `OUT_DIR`,
/// never elevated inside a directory another account owns.
///
/// `std::fs::set_permissions` resolves its path again and follows whatever link
/// it finds. Under an elevated run inside a directory an unprivileged user owns,
/// that is a read-anything primitive: unlink the file cfgd just wrote, plant a
/// link at another user's private key, and root applies the mode to that
/// instead. So a site either takes [`crate::set_file_permissions_nofollow`] /
/// [`crate::widen_file_permissions_nofollow`], which chmod a descriptor and
/// refuse a symlink outright, or carries a `// follow-ok: <why>` line on its own
/// line or the line immediately above it, which is where this repo binds every
/// other hatch: a wider window lets a marker written for one site drift above a
/// second site and silently excuse it.
///
/// What counts and what offends are deliberately different sets.
/// [`ChmodPopulation::chmods`] counts EVERY chmod-shaped call the walk read,
/// no-follow ones included, because the follow-capable sites are the ones this
/// rule drives to zero and flooring on those alone would turn a fully converted
/// crate into a failure. Only the two path-based spellings can be misdirected,
/// so only they are asked the question. A chmod through a descriptor
/// (`file.set_permissions(…)` on a handle the caller opened) cannot be pointed
/// at a second file and is in neither set, and a `set_mode` on a `Permissions`
/// value reaches the filesystem only through one of the calls already judged. A
/// `.mode(0o…)` on an `OpenOptions` is outside both sets too (and outside the
/// class): a create-with-mode that follows a planted link either writes the
/// victim, which is `atomic_write`'s question, or creates cfgd's own file, and
/// either way no existing file's mode moves. A
/// COMMENT line counts for nothing either way: a doc sentence naming the
/// primitive is documentation, not a call site, and a floor a rustdoc paragraph
/// could hold up would let the real population shrink with the walk none the
/// wiser. A function DECLARATION is skipped on the same grounds, and so is a
/// tell inside a STRING LITERAL, and so is a source that IS test scaffolding.
pub struct ChmodPopulation {
    /// Crate `src` roots the walk read, workspace-relative.
    ///
    /// The NAMES rather than a count, so a caller can fail by name when a root
    /// is renamed or moved out of `crates/`: a count is restored by any crate
    /// that happens to appear, and the walk then reads a narrower population in
    /// silence.
    pub roots: Vec<String>,
    /// Production sources the walk read.
    pub files: usize,
    /// Chmod-shaped calls it read, no-follow ones included.
    pub chmods: usize,
    /// Path-based chmods with no `// follow-ok:` above them, `<rel>:<line>` first.
    pub offenders: Vec<String>,
}

/// Walk every `<crate>/src` under `crates_dir` for [`ChmodPopulation`]. Offender
/// lines are workspace-relative, so one naming a file says which crate holds it.
pub fn path_based_chmod_population(crates_dir: &Path) -> ChmodPopulation {
    // A tell inside a string literal is a message NAMING the primitive, not a
    // call to it: `tracing::debug!("set_file_permissions is a no-op on Windows")`
    // would otherwise hold this walk's floor up and be asked for a hatch a log
    // line cannot carry.
    fn names_outside_a_literal(line: &str, tell: &str) -> bool {
        let mut quoted = false;
        let mut chars = line.char_indices();
        while let Some((idx, ch)) = chars.next() {
            if !quoted && line[idx..].starts_with(tell) {
                return true;
            }
            match ch {
                '\\' if quoted => {
                    chars.next();
                }
                '"' => quoted = !quoted,
                _ => {}
            }
        }
        false
    }
    const COUNTED: &[&str] = &[
        "set_file_permissions",
        "widen_file_permissions",
        "fs::set_permissions(",
        "widen_world_readable(",
        "carry_dir_mode(",
    ];
    let mut population = ChmodPopulation {
        roots: Vec::new(),
        files: 0,
        chmods: 0,
        offenders: Vec::new(),
    };
    let mut roots: Vec<std::path::PathBuf> = std::fs::read_dir(crates_dir)
        .unwrap_or_else(|e| panic!("{}: {e}", crates_dir.display()))
        .map(|entry| {
            entry
                .unwrap_or_else(|e| {
                    panic!(
                        "{}: the walk must read every entry: {e}",
                        crates_dir.display()
                    )
                })
                .path()
                .join("src")
        })
        .filter(|src| src.is_dir())
        .collect();
    roots.sort();
    let workspace = workspace_root();
    population.roots = roots
        .iter()
        .map(|root| crate::to_posix_string(root.strip_prefix(&workspace).unwrap_or(root)))
        .collect();
    for path in roots.iter().flat_map(|root| rust_sources_under(root)) {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        // A file that IS test scaffolding carries no `#[cfg(test)]` of its own
        // for the slice to cut at, so it is named out here instead.
        if name.starts_with("tests")
            || name == "test_helpers.rs"
            || path.parent().is_some_and(|p| p.ends_with("tests"))
        {
            continue;
        }
        let body = production_slice_of(&path);
        population.files += 1;
        let relative = crate::to_posix_string(path.strip_prefix(&workspace).unwrap_or(&path));
        let lines: Vec<&str> = body.lines().collect();
        for (idx, line) in lines.iter().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            // A function DECLARATION carries the name of the primitive it is,
            // not a call to it: `pub fn set_file_permissions(` is the chmod
            // every judged site reaches, and asking it the question would ask
            // the rule of itself.
            if line.contains(" fn ") || line.trim_start().starts_with("fn ") {
                continue;
            }
            if COUNTED
                .iter()
                .any(|tell| names_outside_a_literal(line, tell))
            {
                population.chmods += 1;
            }
            if !(names_outside_a_literal(line, "set_file_permissions(")
                || names_outside_a_literal(line, "fs::set_permissions("))
            {
                continue;
            }
            if lines[idx.saturating_sub(1)..=idx]
                .iter()
                .any(|l| l.contains("follow-ok:"))
            {
                continue;
            }
            population.offenders.push(format!(
                "{relative}:{}: chmods a path that may be a symlink, take \
                 `set_file_permissions_nofollow` (or \
                 `widen_file_permissions_nofollow`), else mark it \
                 `// follow-ok: <the ownership fact that makes the follow safe>`",
                idx + 1
            ));
        }
    }
    population
}

/// Every snapshot-golden root in the workspace, workspace-relative.
///
/// Named rather than only derived: a derivation alone shrinks in silence when
/// a root is renamed or moved, and every floor over the population still
/// passes because the surviving roots hold goldens. [`snapshot_golden_roots`]
/// fails by name instead, and the fences walk holds the other direction — a
/// root that exists and is not named here.
pub const KNOWN_GOLDEN_ROOTS: &[&str] = &[
    "crates/cfgd-core/src/oci/snapshots",
    "crates/cfgd-core/src/output/tests/snapshots",
    "crates/cfgd-core/src/reconciler/snapshots",
    "crates/cfgd-core/src/server_client/snapshots",
    "crates/cfgd-core/tests/golden",
    "crates/cfgd-core/tests/snapshots",
    "crates/cfgd/src/packages/brew/snapshots",
    "crates/cfgd/src/system/node/snapshots",
    "crates/cfgd/src/system/snapshots",
    "crates/cfgd/src/system/ssh_keys/snapshots",
    "crates/cfgd/tests/output_snapshots",
];

/// Every directory under `crates/` named `snapshots`, `output_snapshots` or
/// `golden`, sorted, with every root [`KNOWN_GOLDEN_ROOTS`] names asserted
/// present.
///
/// The ONE derivation of the golden population for the whole workspace. The
/// crates compile separately, so a walk that re-derived the roots in its own
/// crate read a NARROWER population than its sibling did and the two
/// disagreed about which goldens are guarded at all — which is what a walk
/// over "every golden" exists to rule out. `target/` is skipped: a build tree
/// mirrors captured renders under paths nobody ships. A root is not descended
/// into, so a nested one is never listed twice.
///
/// The directory NAME is the whole guard, so a tree of committed goldens goes
/// under one of the three names above; one named anything else is outside this
/// derivation, outside [`KNOWN_GOLDEN_ROOTS`] and outside every walk reading
/// either, with nothing to say so.
pub fn snapshot_golden_roots() -> Vec<PathBuf> {
    let root = workspace_root();
    let mut roots = Vec::new();
    let mut stack = vec![root.join("crates")];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).unwrap_or_else(|e| {
            panic!("{}: the walk must read every directory: {e}", dir.display())
        });
        for entry in entries {
            let entry = entry.unwrap_or_else(|e| {
                panic!("{}: the walk must read every entry: {e}", dir.display())
            });
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            match path.file_name().and_then(|n| n.to_str()) {
                Some("target") => {}
                Some("snapshots" | "output_snapshots" | "golden") => roots.push(path),
                _ => stack.push(path),
            }
        }
    }
    for known in KNOWN_GOLDEN_ROOTS {
        assert!(
            roots.contains(&root.join(known)),
            "the walk no longer derives {known}; it has been renamed or moved, \
             and the goldens under it are unguarded"
        );
    }
    roots.sort();
    roots
}

/// Every file under every [`snapshot_golden_roots`] root, sorted.
///
/// The goldens are the `.txt` half of this; the rest is what a walk states
/// the COMPLEMENT of, so a render captured under a new extension is
/// classified rather than skipped in silence.
pub fn snapshot_root_files() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = snapshot_golden_roots()
        .iter()
        .flat_map(|root| files_under_root(root))
        .collect();
    files.sort();
    files
}

/// Every snapshot golden in the workspace carrying one of `exts`, sorted.
///
/// The `-o json` payloads sit beside the rendered `.txt` captures under the
/// same roots, so a walk names the extensions its own claim is about.
pub fn snapshot_goldens(exts: &[&str]) -> Vec<PathBuf> {
    snapshot_root_files()
        .into_iter()
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| exts.contains(&e))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::FileManager;
    use secrecy::ExposeSecret;

    /// The shapes the fold has to tell apart, in one source.
    ///
    /// A raw literal is the one a naive "trailing backslash continues the
    /// line" rule gets WRONG in the dangerous direction: it joins two lines
    /// that were never one, and a walk then reports a tell on a line that does
    /// not carry it. An escaped backslash is the same mistake at the end of an
    /// ordinary literal. The hashed and byte-raw forms take the hash-counting
    /// arithmetic in `scan_raw_literals`, which the zero-hash case never
    /// touches, so each holds a case of its own.
    #[test]
    fn the_continuation_fold_joins_only_a_real_continuation() {
        let body = concat!(
            "let a = \"one \\\n",
            "    two\";\n",
            "let b = r\"a raw line ending in \\\n",
            "    and its next line\";\n",
            "let c = \"escaped \\\\\";\n",
            "let d = r#\"a hashed raw ending in \\\n",
            "    still raw\"#;\n",
            "let e = br\"a byte raw ending in \\\n",
            "    and its next line\";\n",
            "let f = r##\"holds a \"# decoy and ends in \\\n",
            "    still raw\"##;\n"
        );
        let folded = logical_source_lines(body);
        assert_eq!(
            folded.len(),
            10,
            "only the first literal is continued: {folded:?}"
        );
        assert_eq!(folded[0].0, 1, "a fold reports its OPENING line");
        assert_eq!(
            folded[0].1, "let a = \"one two\";",
            "a real continuation joins onto the line that opened it, its indent eaten"
        );
        assert_eq!(folded[1].0, 3);
        assert!(
            folded[1].1.ends_with('\\'),
            "a raw literal has no continuations, so its line stands alone and \
             keeps its backslash: {folded:?}"
        );
        assert_eq!(folded[2].0, 4, "the raw literal's next line is its own");
        assert_eq!(folded[3].0, 5);
        assert!(
            folded[3].1.ends_with("\\\\\";"),
            "an escaped backslash ends the literal it is in: {folded:?}"
        );
        assert_eq!(folded[4].0, 6);
        assert!(
            folded[4].1.ends_with('\\'),
            "a hashed raw literal suppresses the fold like a bare one: {folded:?}"
        );
        assert_eq!(folded[5].0, 7, "the hashed raw's next line is its own");
        assert_eq!(folded[6].0, 8);
        assert!(
            folded[6].1.ends_with('\\'),
            "a byte-raw literal suppresses the fold like a bare one: {folded:?}"
        );
        assert_eq!(folded[7].0, 9, "the byte raw's next line is its own");
        assert_eq!(folded[8].0, 10);
        assert!(
            folded[8].1.ends_with('\\'),
            "a two-hash raw literal stays open past a one-hash `\"#` decoy, \
             so its line stands alone: {folded:?}"
        );
        assert_eq!(folded[9].0, 11, "the two-hash raw's next line is its own");
    }

    #[test]
    fn production_slice_cuts_at_the_trailing_test_module_not_an_early_attribute() {
        // The attribute is composed rather than written out: a literal one here
        // is a `#[cfg(test)]` at the start of a line, which the audit's own
        // test-block stripper would take for this file's test module.
        let attr = format!("#[cfg({})]", "test");
        let src = format!(
            "use a::b;\n{attr}\nuse c::d;\n\
             pub const RENDERED: &str = \"a literal a walk must see\";\n\
             {attr}\nmod tests_inline #OPEN#\n    fn hidden() #OPEN##CLOSE#\n#CLOSE#\n"
        )
        .replace("#OPEN#", "{")
        .replace("#CLOSE#", "}");
        let production = production_slice(&src);
        assert!(
            production.contains("a literal a walk must see"),
            "the cut must keep the production code between an early \
             `#[cfg(test)]` import and the trailing test module: {production}"
        );
        assert!(
            !production.contains("fn hidden"),
            "the trailing test module must be cut away: {production}"
        );
        assert_eq!(
            production_slice("pub fn only_production() -> u8 { 1 }\n"),
            "pub fn only_production() -> u8 { 1 }\n",
            "a file with no test module comes back whole"
        );
    }

    /// The shape of an aggregator `mod.rs`: `mod tests;` is a one-line
    /// DECLARATION sitting beside its sibling `mod x;` lines, and the file's
    /// real content follows it. Cutting there is the blinding the helper exists
    /// to prevent, so the whole source survives.
    #[test]
    fn production_slice_keeps_a_file_whose_test_module_is_a_mid_file_declaration() {
        let attr = format!("#[cfg({})]", "test");
        let src = format!(
            "mod plan;\nmod verify;\n{attr}\nmod tests;\n\
             pub use plan::Plan;\n\
             pub const RENDERED: &str = \"a literal a walk must see\";\n"
        );
        assert_eq!(
            production_slice(&src),
            src,
            "a `mod tests;` declaration is not the end of the file, so nothing \
             below it may be cut away"
        );
    }

    /// Both shapes in one file: the declaration is skipped and the search
    /// continues to the inline block, which is where the cut lands.
    #[test]
    fn production_slice_cuts_at_the_inline_block_past_a_mid_file_declaration() {
        let attr = format!("#[cfg({})]", "test");
        let src = format!(
            "mod plan;\n{attr}\nmod tests;\n\
             pub const RENDERED: &str = \"a literal a walk must see\";\n\
             {attr}\nmod inline_tests #OPEN#\n    fn hidden() #OPEN##CLOSE#\n#CLOSE#\n"
        )
        .replace("#OPEN#", "{")
        .replace("#CLOSE#", "}");
        let production = production_slice(&src);
        assert!(
            production.contains("a literal a walk must see"),
            "the code between the declaration and the inline block must \
             survive: {production}"
        );
        assert!(
            production.contains("mod tests;"),
            "the declaration itself is production text, not a cut point: \
             {production}"
        );
        assert!(
            !production.contains("fn hidden"),
            "the inline test module is where the cut lands: {production}"
        );
    }

    /// The strip is not a suffix cut. Here the inline block comes FIRST and a
    /// bare `mod tests;` declaration is the rightmost anchor, so anything that
    /// searches backward for one cut point either stops at the declaration
    /// (dropping the file) or cuts at the block (dropping everything after it).
    #[test]
    fn production_slice_strips_an_inline_block_that_is_not_the_last_anchor() {
        let attr = format!("#[cfg({})]", "test");
        let src = format!(
            "{attr}\nmod early_tests #OPEN#\n    fn hidden() #OPEN##CLOSE#\n#CLOSE#\n\
             pub const RENDERED: &str = \"a literal a walk must see\";\n\
             {attr}\nmod tests;\n\
             pub const TRAILING: &str = \"below the declaration\";\n"
        )
        .replace("#OPEN#", "{")
        .replace("#CLOSE#", "}");
        let production = production_slice(&src);
        assert!(
            !production.contains("fn hidden"),
            "an inline block is stripped wherever it sits: {production}"
        );
        for kept in ["a literal a walk must see", "below the declaration"] {
            assert!(
                production.contains(kept),
                "production after an earlier inline block must survive: \
                 {production}"
            );
        }
    }

    /// Several inline siblings in one file — the shape `output/mod.rs` has —
    /// where a suffix cut leaves every block but the last inside the slice.
    #[test]
    fn production_slice_strips_every_inline_sibling() {
        let attr = format!("#[cfg({})]", "test");
        let src = format!(
            "{attr}\nmod first #OPEN#\n    fn hidden_one() #OPEN##CLOSE#\n#CLOSE#\n\
             pub const A: &str = \"between one and two\";\n\
             {attr}\nmod second #OPEN#\n    fn hidden_two() #OPEN##CLOSE#\n#CLOSE#\n\
             pub const B: &str = \"between two and three\";\n\
             {attr}\nmod third #OPEN#\n    fn hidden_three() #OPEN##CLOSE#\n#CLOSE#\n"
        )
        .replace("#OPEN#", "{")
        .replace("#CLOSE#", "}");
        let production = production_slice(&src);
        for gone in ["hidden_one", "hidden_two", "hidden_three"] {
            assert!(
                !production.contains(gone),
                "every inline sibling is stripped, not just the last: \
                 {production}"
            );
        }
        for kept in ["between one and two", "between two and three"] {
            assert!(
                production.contains(kept),
                "the production between the siblings survives: {production}"
            );
        }
    }

    /// A platform-gated test module stacks a second attribute
    /// (`#[cfg(unix)]`) between `#[cfg(test)]` and its `mod tests {` line —
    /// the shape `daemon/health_ipc.rs`, both `daemon/service/*.rs` files and
    /// `cli/kubectl.rs` carry. Checking only the IMMEDIATE next line for
    /// `mod ` missed it, leaking a whole test module into every walk reading
    /// through this helper.
    #[test]
    fn production_slice_skips_a_stacked_platform_attribute_before_the_mod_line() {
        let attr = format!("#[cfg({})]", "test");
        let src = format!(
            "pub const RENDERED: &str = \"a literal a walk must see\";\n\
             {attr}\n#[cfg(unix)]\nmod tests #OPEN#\n    fn hidden() #OPEN##CLOSE#\n#CLOSE#\n"
        )
        .replace("#OPEN#", "{")
        .replace("#CLOSE#", "}");
        let production = production_slice(&src);
        assert!(
            production.contains("a literal a walk must see"),
            "production ahead of the stacked-attribute module survives: {production}"
        );
        assert!(
            !production.contains("fn hidden"),
            "the module behind a stacked platform attribute is still cut away: {production}"
        );
    }

    /// The guard's exclusion is the reason no window-pinning test carries a
    /// serial attribute for it, so the exclusion has to be observable rather
    /// than assumed. Every assertion here is made from INSIDE this test's own
    /// guard: whether the lock is FREE is never this test's to claim, because
    /// any of the window tests may hold it at any moment, and asserting on it
    /// makes this test fail for someone else's correct behaviour.
    #[test]
    fn a_live_window_pin_holds_the_lock_that_keeps_a_second_pin_out() {
        let pinned = GitRefreshWindowGuard::never_expires();
        assert!(
            GIT_REFRESH_WINDOW_PIN.try_lock().is_err(),
            "a live pin must hold the lock, or two tests can pin the window at once"
        );
        drop(pinned);
        // The release is observed by taking the exclusion again rather than by
        // asking whether the lock is free — a guard that failed to release
        // would leave this acquisition unsatisfiable on the same thread.
        let _second = GitRefreshWindowGuard::never_expires();
        assert!(
            GIT_REFRESH_WINDOW_PIN.try_lock().is_err(),
            "the second pin must hold the lock the first one released"
        );
    }

    /// Concurrent dispatch is made of a reader that cannot leave its critical
    /// section until a SECOND reader enters it: a lane worker blocked in a
    /// fixture rendezvous holds the read side, and the thread waiting on it
    /// cannot proceed until the sibling worker — a fresh thread, so a real
    /// acquisition — starts. Queue a writer between the two and a
    /// write-preferring lock deadlocks all four parties until a test-side
    /// timeout expires minutes later, having stalled every other reader in the
    /// binary behind the same writer. The gate admits the sibling instead: the
    /// writer is already waiting on the reader inside, and refusing the
    /// newcomer buys it nothing.
    #[test]
    fn a_queued_writer_does_not_shut_out_a_reader_joining_one_already_inside() {
        let (holder_in, holder_ready) = std::sync::mpsc::channel();
        let (release_holder, holder_waits) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            let _inside = path_env_read_guard();
            holder_in.send(()).expect("report the read side taken");
            holder_waits.recv().expect("wait for the release");
        });
        holder_ready.recv().expect("the read side is held");

        let writer = std::thread::spawn(|| {
            let _exclusive = path_env_mutation_guard();
        });
        // The writer is QUEUED, not merely spawned — the state a clock would
        // otherwise be guessing at.
        assert!(
            await_queued_path_writer(std::time::Duration::from_secs(10)),
            "the writer never reached the gate"
        );

        let (joined_in, joined) = std::sync::mpsc::channel();
        let joiner = std::thread::spawn(move || {
            let _inside = path_env_read_guard();
            joined_in.send(()).expect("report the second read taken");
        });
        joined
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("a reader joining one already inside was shut out by a queued writer");

        release_holder.send(()).expect("release the first reader");
        for thread in [holder, writer, joiner] {
            thread.join().expect("thread");
        }
    }

    /// The spawn-environment guards must compose: a test that pins the working
    /// directory *and* puts a shim on `PATH` is natural, and both halves take
    /// the exclusive guard. Without per-thread re-entrancy the second
    /// acquisition queues behind the first and hangs the suite with no
    /// timeout, so this test only ever passes or never returns.
    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn spawn_env_guards_compose_without_deadlocking() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _cwd = CwdGuard::set(dir.path()).expect("cwd guard");
        let (_shim_dir, _shim) = install_named_path_shim("cfgd-fake-tool", 0, "", "");
        let _nested_cwd = CwdGuard::set(dir.path()).expect("nested cwd guard");
        let _nested_excl = path_env_mutation_guard();
        // A spawn inside the window degrades to a no-op guard rather than
        // blocking on the exclusive holder's own lock.
        let _spawn = path_env_read_guard();
        assert_eq!(
            std::fs::canonicalize(std::env::current_dir().expect("cwd")).expect("canonicalize"),
            std::fs::canonicalize(dir.path()).expect("canonicalize"),
            "the innermost guard still owns the working directory"
        );
    }

    /// The one order re-entrancy cannot rescue: a read guard cannot upgrade to
    /// a write guard. Without the assertion this hangs; with it the misuse is a
    /// loud panic naming the fix.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "while holding the shared spawn guard")]
    fn taking_the_exclusive_guard_inside_a_spawn_guard_panics() {
        let _shared = path_env_read_guard();
        let _exclusive = path_env_mutation_guard();
    }

    #[test]
    fn mock_file_manager_records_calls() {
        let fm = MockFileManager::new();
        let layers = vec![FileLayer {
            source_dir: PathBuf::from("/tmp/src"),
            origin_source: "test-origin".into(),
            priority: 0,
        }];
        let printer = test_printer();

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
        let printer = test_printer();
        fm.set_fail_apply(true);
        let result = fm.apply(&[], &printer);
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("mock-failure"),
            "expected mock-failure path in error, got: {err_msg}"
        );
    }

    #[test]
    fn mock_file_manager_content_drift_derives_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src.txt");
        let matching = dir.path().join("same.txt");
        let drifted = dir.path().join("diff.txt");
        std::fs::write(&source, "hello").unwrap();
        std::fs::write(&matching, "hello").unwrap();
        std::fs::write(&drifted, "tampered").unwrap();

        let fm = MockFileManager::new();

        let ok = fm.content_drift(&source, &matching, None, None).unwrap();
        assert!(ok.matches);
        assert_eq!(ok.actual, "content matches source");

        let bad = fm.content_drift(&source, &drifted, None, None).unwrap();
        assert!(!bad.matches);
        assert!(bad.actual.contains("differs"));

        let missing = fm
            .content_drift(&source, &dir.path().join("nope.txt"), None, None)
            .unwrap();
        assert!(!missing.matches);
        assert_eq!(missing.actual, "missing");

        assert_eq!(fm.content_drift_calls.lock().unwrap().len(), 3);
    }

    #[test]
    fn mock_file_manager_content_drift_returns_pinned_result() {
        let fm = MockFileManager::new();
        fm.set_content_drift_result(FileDriftResult {
            target: "~/.bashrc".to_string(),
            matches: false,
            expected: "content matches source".to_string(),
            actual: "content differs from source".to_string(),
            unmanaged: false,
        });

        let result = fm
            .content_drift(
                Path::new("/does/not/exist"),
                Path::new("/also/missing"),
                None,
                None,
            )
            .unwrap();
        assert_eq!(result.target, "~/.bashrc");
        assert!(!result.matches);
        assert!(result.actual.contains("differs"));
    }

    #[test]
    fn mock_file_manager_content_drift_reports_missing_source() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("present.txt");
        std::fs::write(&target, "managed").unwrap();

        let fm = MockFileManager::new();
        let result = fm
            .content_drift(&dir.path().join("absent-source.txt"), &target, None, None)
            .unwrap();

        assert!(!result.matches, "absent managed source must report drift");
        assert_eq!(result.expected, "managed source present");
        assert_eq!(result.actual, "source not found");
    }

    #[test]
    fn mock_secret_backend_unavailable_and_records_write_calls() {
        let backend = MockSecretBackend::new("age").unavailable();
        assert_eq!(backend.name(), "age");
        assert!(
            !backend.is_available(),
            "`unavailable()` must flip availability off"
        );

        backend.encrypt_file(Path::new("/tmp/plain.yaml")).unwrap();
        backend.edit_file(Path::new("/tmp/edit.yaml")).unwrap();
        assert_eq!(
            backend.encrypt_calls.lock().unwrap().as_slice(),
            &[PathBuf::from("/tmp/plain.yaml")]
        );
        assert_eq!(
            backend.edit_calls.lock().unwrap().as_slice(),
            &[PathBuf::from("/tmp/edit.yaml")]
        );
    }

    #[test]
    fn mock_system_configurator_failing_diff_errors() {
        let cfg = MockSystemConfigurator::new("sysctl").failing();
        match cfg.diff(&serde_yaml::Value::Null) {
            Ok(_) => panic!("failing() must make diff error"),
            Err(err) => assert!(format!("{err}").contains("mock diff failed")),
        }
    }

    #[test]
    fn test_env_builder_default_matches_new() {
        // `default()` simply forwards to `new()`; building from it must yield a
        // usable env with the same directory layout.
        let env = TestEnvBuilder::default().build();
        assert!(env.config_dir.exists());
        assert!(env.profiles_dir.exists());
        assert!(env.modules_dir.exists());
        // `TestEnv::path` joins onto the root without touching disk.
        let joined = env.path("sub/leaf.txt");
        assert_eq!(joined, env.root.join("sub/leaf.txt"));
        assert!(!env.file_exists("sub/leaf.txt"));
    }

    #[test]
    fn assert_snapshot_golden_regenerates_missing_golden() {
        let dir = tempfile::tempdir().unwrap();
        let name = "nested/out.txt";
        // Golden does not yet exist → the regen branch writes it and returns
        // without asserting, creating the parent dir along the way.
        assert_snapshot_golden(
            dir.path(),
            name,
            "regenerated body\n",
            env!("CARGO_PKG_VERSION"),
        );
        let written = std::fs::read_to_string(dir.path().join(name)).unwrap();
        assert_eq!(written, "regenerated body\n");

        // A matching second call now takes the compare branch without panicking.
        assert_snapshot_golden(
            dir.path(),
            name,
            "regenerated body\n",
            env!("CARGO_PKG_VERSION"),
        );
    }

    #[test]
    #[serial_test::serial]
    fn editor_guard_drop_removes_var_when_no_prior() {
        // SAFETY: serial gates env mutation across tests.
        unsafe {
            std::env::remove_var("EDITOR");
        }
        {
            let _guard = EditorGuard::set("vi");
            assert_eq!(std::env::var("EDITOR").as_deref(), Ok("vi"));
        }
        assert!(
            std::env::var("EDITOR").is_err(),
            "drop must remove EDITOR when none was set before"
        );
    }

    #[test]
    fn mock_package_manager_helpers_and_trait_methods() {
        use crate::providers::{PackageManager, PackageManagerExt};

        let mgr = MockPackageManager::new("pacman")
            .with_installed(&["git"])
            .unavailable()
            .bootstrappable();
        let printer = test_printer();
        let state = test_state();
        let cx = test_package_context(&printer, &state);

        assert_eq!(mgr.name(), "pacman");
        assert!(
            !mgr.is_available(),
            "`unavailable()` must flip availability"
        );
        assert!(
            mgr.can_bootstrap(),
            "`bootstrappable()` must enable bootstrap"
        );
        mgr.bootstrap(&cx).unwrap();

        assert_eq!(
            mgr.installed_packages(&cx).unwrap(),
            std::collections::HashSet::from(["git".to_string()])
        );

        mgr.install(&["vim".to_string()], &cx).unwrap();
        mgr.uninstall(&["nano".to_string()], &cx).unwrap();
        mgr.refresh_index(&cx).unwrap();
        assert_eq!(
            mgr.install_calls.lock().unwrap().as_slice(),
            &[vec!["vim".to_string()]]
        );
        assert_eq!(
            mgr.uninstall_calls.lock().unwrap().as_slice(),
            &[vec!["nano".to_string()]]
        );

        assert_eq!(mgr.available_version("anything").unwrap(), None);
    }

    #[test]
    fn reconciler_harness_builder_accepts_custom_file_manager() {
        // `file_manager()` overrides the default mock; the override must be wired
        // into the registry such that a plan can be produced from it.
        let harness = ReconcilerTestHarness::builder()
            .file_manager(MockFileManager::new())
            .build();
        let plan = harness
            .plan()
            .expect("custom file manager must yield a plan");
        assert!(
            plan.is_empty(),
            "empty profile must produce a plan with no actions"
        );
    }

    #[test]
    fn parse_profile_yaml_accepts_full_document_form() {
        // The document branch (apiVersion/kind/metadata/spec) is taken before the
        // bare-spec fallback; a full ProfileDocument must parse via `doc.spec`.
        let harness = ReconcilerTestHarness::builder()
            .profile_yaml(
                "apiVersion: cfgd.io/v1\n\
                 kind: Profile\n\
                 metadata:\n  name: doc-form\n\
                 spec:\n  modules: []\n",
            )
            .build();
        assert!(
            harness.resolved.merged.modules.is_empty(),
            "document-form profile with no modules must resolve to an empty module set"
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
        let printer = test_printer();
        let desired = serde_yaml::Value::String("test".into());
        sc.apply(&desired, &crate::providers::SystemContext::new(&printer))
            .unwrap();
        assert_eq!(sc.apply_calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn mock_system_configurator_can_fail() {
        let sc = MockSystemConfigurator::new("sysctl");
        let printer = test_printer();
        sc.set_fail_apply(true);
        let result = sc.apply(
            &serde_yaml::Value::Null,
            &crate::providers::SystemContext::new(&printer),
        );
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
    // BareGitRepo
    // -----------------------------------------------------------------------

    #[test]
    fn bare_git_repo_clone_from_fixture() {
        let repo = BareGitRepo::builder()
            .commit("initial", &[("README.md", "hello")])
            .commit(
                "second",
                &[("module.yaml", "apiVersion: cfgd.io/v1alpha1\n")],
            )
            .tag("v1.0.0")
            .branch("feature", &[("extra.txt", "data")])
            .build();

        // Verify URL is file:// protocol
        let url = repo.url();
        assert!(
            url.starts_with("file://"),
            "url should be file://, got: {url}"
        );

        // Verify tags and branches exist
        assert!(repo.has_tag("v1.0.0"));
        assert!(!repo.has_tag("v2.0.0"));
        assert!(repo.has_branch("feature"));
        assert!(repo.has_branch(repo.head_branch()));

        // Clone from the bare repo and verify contents
        let clone_dir = tempfile::TempDir::new().unwrap();
        let cloned = git2::Repository::clone(&url, clone_dir.path()).unwrap();

        // Verify files from commits are present. `read_to_string` returns the
        // on-disk bytes — on a Windows git checkout with default
        // `core.autocrlf=true`, that is CRLF even though LF was committed.
        // Compare after `normalize_line_endings` so the assertion is about
        // logical content, not the OS-specific eol translation policy.
        let readme = std::fs::read_to_string(clone_dir.path().join("README.md")).unwrap();
        assert_eq!(crate::normalize_line_endings(&readme), "hello");
        let module = std::fs::read_to_string(clone_dir.path().join("module.yaml")).unwrap();
        assert_eq!(
            crate::normalize_line_endings(&module),
            "apiVersion: cfgd.io/v1alpha1\n"
        );

        // Verify commit history (2 commits on main)
        let head = cloned.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(head.message().unwrap(), "second");
        let parent = head.parent(0).unwrap();
        assert_eq!(parent.message().unwrap(), "initial");
    }

    #[test]
    fn bare_git_repo_fetch_branch_from_fixture() {
        let repo = BareGitRepo::builder()
            .commit("base", &[("base.txt", "base content")])
            .branch("dev", &[("dev.txt", "dev content")])
            .build();

        // Clone only the main branch
        let clone_dir = tempfile::TempDir::new().unwrap();
        let cloned = git2::Repository::clone(&repo.url(), clone_dir.path()).unwrap();

        // Fetch the dev branch
        let mut remote = cloned.find_remote("origin").unwrap();
        remote
            .fetch(&["refs/heads/dev:refs/remotes/origin/dev"], None, None)
            .unwrap();

        // Checkout origin/dev
        let dev_ref = cloned.find_reference("refs/remotes/origin/dev").unwrap();
        let dev_commit = dev_ref.peel_to_commit().unwrap();

        // The branch commit should contain the extra file
        let dev_tree = dev_commit.tree().unwrap();
        assert!(
            dev_tree.get_name("dev.txt").is_some(),
            "dev branch should contain dev.txt"
        );
        assert!(
            dev_tree.get_name("base.txt").is_some(),
            "dev branch should also contain base.txt"
        );

        // Verify tag listing
        let tags = repo.tags();
        assert!(tags.is_empty(), "no tags were added");
    }

    #[test]
    fn bare_git_repo_multiple_tags() {
        let repo = BareGitRepo::builder()
            .commit("first", &[("a.txt", "a")])
            .tag("v0.1.0")
            .commit("second", &[("b.txt", "b")])
            .tag("v0.2.0")
            .build();

        assert!(repo.has_tag("v0.1.0"));
        assert!(repo.has_tag("v0.2.0"));
        assert!(!repo.has_tag("v0.3.0"));

        let tags = repo.tags();
        assert_eq!(tags.len(), 2);
        assert!(tags.contains(&"v0.1.0".to_string()));
        assert!(tags.contains(&"v0.2.0".to_string()));
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

    // -----------------------------------------------------------------------
    // ReconcilerTestHarness
    // -----------------------------------------------------------------------

    mod reconciler_test_harness {
        use super::super::*;
        use crate::providers::PackageAction;
        use crate::state::ApplyStatus;
        use secrecy::ExposeSecret;

        #[test]
        fn harness_plan_empty_profile_produces_no_phases() {
            let h = ReconcilerTestHarness::builder()
                .package_manager("brew", &["curl", "git"])
                .system_configurator("shell", &[])
                .build();

            let plan = h.plan().unwrap();
            // Action-less phases are dropped, so an empty profile plans nothing.
            assert_eq!(plan.phases.len(), 0);
            assert!(plan.is_empty());
        }

        #[test]
        fn harness_apply_empty_plan_succeeds() {
            let h = ReconcilerTestHarness::builder()
                .package_manager("brew", &["curl", "git"])
                .build();

            let plan = h.plan().unwrap();
            let printer = test_printer();
            let result = h.apply(&plan, &printer).unwrap();

            assert_eq!(result.status, ApplyStatus::Success);
            assert_eq!(result.action_results.len(), 0);
        }

        #[test]
        fn harness_plan_with_package_actions() {
            let h = ReconcilerTestHarness::builder()
                .package_manager("brew", &["curl"])
                .build();

            let pkg_actions = vec![PackageAction::Install {
                manager: "brew".to_string(),
                packages: vec!["ripgrep".to_string()],
                origin: "local".to_string(),
            }];

            let plan = h
                .plan_with_actions(Vec::new(), pkg_actions, Vec::new())
                .unwrap();

            assert!(!plan.is_empty());
            assert_eq!(
                plan.total_actions(),
                2,
                "the install, plus the `Bootstrap` node refreshing the index it reads"
            );
        }

        #[test]
        fn harness_with_secret_provider() {
            let h = ReconcilerTestHarness::builder()
                .secret_provider("1password", "s3cr3t-value")
                .build();

            // The secret provider is wired into the registry
            assert_eq!(h.registry.secret_providers.len(), 1);

            let plan = h.plan().unwrap();
            assert!(plan.is_empty());

            // The provider resolves correctly (verifies wiring)
            let secret = h.registry.secret_providers[0]
                .resolve("op://vault/item/field")
                .unwrap();
            assert_eq!(secret.expose_secret(), "s3cr3t-value");
        }

        #[test]
        fn harness_with_profile_yaml() {
            let yaml = r#"
modules:
  - nvim
env:
  - name: EDITOR
    value: nvim
"#;
            let h = ReconcilerTestHarness::builder()
                .profile_yaml(yaml)
                .package_manager("brew", &[])
                .build();

            assert_eq!(h.resolved_profile().merged.modules, vec!["nvim"]);
            assert_eq!(h.resolved_profile().merged.env.len(), 1);
            assert_eq!(h.resolved_profile().merged.env[0].name, "EDITOR");
        }

        #[test]
        fn harness_apply_records_in_state_store() {
            let h = ReconcilerTestHarness::builder().build();

            let plan = h.plan().unwrap();
            let printer = test_printer();
            let result = h.apply(&plan, &printer).unwrap();

            assert_eq!(result.status, ApplyStatus::Success);

            // State store should have recorded the apply
            let history = h.state_store().history(10).unwrap();
            assert_eq!(history.len(), 1);
        }

        #[test]
        fn harness_plan_with_system_configurator_drift() {
            use crate::providers::SystemDrift;

            let drift = SystemDrift {
                key: "net.ipv4.ip_forward".into(),
                expected: "1".into(),
                actual: "0".into(),
            };

            let h = ReconcilerTestHarness::builder()
                .system_configurator_with_drift("sysctl", vec![drift])
                .build();

            // The configurator is wired in
            assert_eq!(h.registry.system_configurators().len(), 1);

            // Plan still works (system drift doesn't automatically generate actions
            // without matching profile system config), so it yields no phases.
            let plan = h.plan().unwrap();
            assert_eq!(plan.phases.len(), 0);
        }

        #[test]
        fn mock_package_manager_records_install_calls() {
            use crate::providers::PackageManager;

            let pm = super::super::MockPackageManager::new("brew").with_installed(&["curl", "git"]);

            assert!(pm.is_available());
            assert_eq!(pm.name(), "brew");

            let printer = test_printer();
            let state = super::super::test_state();
            let cx = super::super::test_package_context(&printer, &state);

            let installed = pm.installed_packages(&cx).unwrap();
            assert!(installed.contains("curl"));
            assert!(installed.contains("git"));
            assert!(!installed.contains("ripgrep"));

            pm.install(&["ripgrep".to_string(), "fd".to_string()], &cx)
                .unwrap();

            let calls = pm.install_calls.lock().unwrap();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0], vec!["ripgrep".to_string(), "fd".to_string()]);
        }

        #[test]
        fn harness_apply_with_context() {
            let h = ReconcilerTestHarness::builder()
                .package_manager("apt", &["vim"])
                .system_configurator("shell", &[])
                .secret_provider("vault", "token-123")
                .build();

            let plan = h.plan().unwrap();
            let printer = test_printer();
            let result = h.apply(&plan, &printer).unwrap();
            assert_eq!(result.status, ApplyStatus::Success);

            // Verify full wiring: all providers present
            assert_eq!(h.registry.package_managers().len(), 1);
            assert_eq!(h.registry.system_configurators().len(), 1);
            assert_eq!(h.registry.secret_providers.len(), 1);
            assert!(h.registry.file_manager.is_some());
        }
    }
}
