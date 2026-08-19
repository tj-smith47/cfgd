//! What a reconcile tick derives from the CONFIG, held for as long as the
//! config it describes has not moved.
//!
//! A tick's work splits in two. One half observes the MACHINE — what is
//! installed, what a file holds, what a system key is set to — and has to run
//! every tick, because that is what drift detection is. The other half derives
//! the DESIRED state from files on disk: parse `cfgd.yaml`, resolve the profile
//! chain, compose the cached sources, build the provider registry, resolve the
//! modules. That half is a pure function of files that mostly never change, and
//! a daemon on a short interval was paying it in full, every tick, forever.
//!
//! Only the second half lives here. Nothing cached here describes the machine,
//! so no cache hit can make a tick miss drift.
//!
//! # What bounds each slot, and why they differ
//!
//! | Slot | Bound |
//! |---|---|
//! | config, profile, composition, registry | the recorded input fingerprint, plus [`CONFIG_REUSE_MAX_AGE`] |
//! | resolved modules | the same, plus [`MODULE_REUSE_TTL`] |
//! | state store | held for the daemon's life |
//!
//! The parse half needs no time bound of its own: a byte-identical file parses
//! to the same object today and in an hour, and the fingerprint is what says it
//! is byte-identical. `CONFIG_REUSE_MAX_AGE` is not there to catch a changed
//! file — it is the ceiling on how long a MISSED input could hide, so a read
//! site that never reported itself costs minutes of staleness rather than the
//! daemon's lifetime.
//!
//! Module resolution is different in kind: it reaches the NETWORK. A module
//! tracking a branch converges because each tick re-resolves it, and a reuse
//! with no ceiling would freeze that module at whatever the first tick fetched.
//! `MODULE_REUSE_TTL` is the same thirty seconds the git refresh window,
//! `command_path` and the enumeration memos carry (see the 30-second convention
//! in `shared-utils.md`), so a daemon ticking at 30s or slower re-resolves on
//! every tick exactly as it did before this cache existed, and one ticking at
//! 5s stops paying it six times a minute.
//!
//! # The fingerprint is `(mtime, len)`, and for source inputs it is alone
//!
//! That pair is the house convention (`packages::ManifestCache` judges its
//! re-reads on the identical one) and it cannot see a same-length rewrite
//! landing inside the filesystem's timestamp granularity. Under the config
//! DIRECTORY that is covered twice over: the daemon's recursive watcher fires
//! on the write itself and invalidates. Inputs read out of the SOURCE cache
//! have no watcher over them, so the fingerprint is their only gate — which is
//! why the sync tick, the one tick that rewrites those checkouts on purpose,
//! invalidates outright rather than trusting a stat to notice.
//!
//! The same asymmetry bounds what a REPLAYED advisory can claim. The composition
//! evaluated its skip conditions against live filesystem state; a reusing tick
//! replays the sentence it recorded, whose truth is only as fresh as the
//! derivation. A `cfgd sync` that fixes the condition normally retires the
//! derivation with it — the checkout directory appears, or the
//! discard-and-reclone recreates it, and either moves a recorded stamp — and
//! `CONFIG_REUSE_MAX_AGE` bounds it regardless. But a replay is a snapshot, not
//! a re-evaluation, and saying a resolved condition one tick too long is the
//! exact inverse of the hazard the replay exists to prevent.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::config::{CfgdConfig, ResolvedProfile};
use crate::modules::{ResolvedModule, SourceModuleRoot};
use crate::providers::ProviderRegistry;
use crate::sources::SourceAdvisory;
use crate::state::StateStore;
use crate::{ConfigInputRecorder, ConfigInputs};

/// Ceiling on how long a parsed config stands without being re-read.
///
/// The fingerprint is the real bound. This is the backstop for the one failure
/// the fingerprint cannot report on itself: a file some reader opens without
/// reporting it as an input. Five minutes bounds that staleness to something a
/// human would call "the next tick or two" instead of "until the daemon
/// restarts", and costs nothing at the default reconcile cadence, which is
/// slower than this.
const CONFIG_REUSE_MAX_AGE: Duration = Duration::from_secs(300);

/// Ceiling on how long a module resolution stands. See the module docs.
const MODULE_REUSE_TTL: Duration = Duration::from_secs(30);

/// The config-derived half of a tick, and the identity it was derived for.
struct ConfigDerivation {
    /// Which config and profile override this was derived for. A cache is per
    /// daemon and these never move under one, but a hit must be judged on them
    /// rather than assumed.
    identity: (PathBuf, Option<String>),
    /// This derivation's id, minted from [`TickCache::next_derivation_id`].
    id: u64,
    /// Behind an `Arc` so a reuse check can take the input set out from under
    /// the slot lock and re-stat it without holding anything.
    inputs: Arc<ConfigInputs>,
    derived_at: Instant,
    cfg: Arc<CfgdConfig>,
    profile_name: String,
    resolved: Arc<ResolvedProfile>,
    source_module_roots: Arc<Vec<SourceModuleRoot>>,
    registry: Arc<ProviderRegistry>,
    source_advisories: Arc<Vec<SourceAdvisory>>,
}

/// The module half, and the config derivation it was resolved against.
struct ModuleDerivation {
    /// The id of the config derivation these modules were resolved against. A
    /// re-derived config means new modules whatever the module inputs say.
    config_derivation_id: u64,
    inputs: Arc<ConfigInputs>,
    resolved_at: Instant,
    modules: Arc<Vec<ResolvedModule>>,
}

/// The objects a daemon derives from its config, held across ticks.
///
/// Shared with the tick through an `Arc` and reached from the blocking thread a
/// tick runs on, so every slot is behind its own lock and no lock is ever held
/// across a derivation.
pub(crate) struct TickCache {
    /// Bumped by [`Self::invalidate`] and by every stored derivation. A
    /// derivation that started before an invalidation is discarded rather than
    /// stored, so an event arriving mid-derivation cannot be lost by the
    /// derivation that was already in flight when it landed.
    epoch: AtomicU64,
    /// Handed to each stored config derivation so a module set can name the one
    /// it was resolved against. Monotonic, never reused — an address would be,
    /// the moment a derivation is dropped and the next allocation lands where
    /// it was.
    next_derivation_id: AtomicU64,
    config: Mutex<Option<ConfigDerivation>>,
    modules: Mutex<Option<ModuleDerivation>>,
    /// The daemon's SQLite handle, opened once. Nothing about a connection goes
    /// stale — it observes the database, not a snapshot of it — so this slot
    /// takes no bound at all, and re-opening it per tick bought nothing but the
    /// pragmas and the migration check `open` runs.
    ///
    /// Lent behind its own lock rather than handed out in an `Arc`, because a
    /// `rusqlite::Connection` is `Send` but NOT `Sync`: an `Arc<StateStore>`
    /// two threads could hold at once would let two of them use one sqlite
    /// handle concurrently. The lock is what makes the lend sound, and the
    /// daemon's ticks are sequential, so nothing ever waits on it.
    ///
    /// Every lend re-checks that the path still names the file the connection
    /// was opened on, and re-opens when it does not — see [`HeldStore`].
    store: Mutex<Option<HeldStore>>,
    /// What each manager reported installed, lent to every tick's
    /// `PackageContext` instead of re-enumerated per tick. Bounded inside
    /// itself by the resolution generation and a 30s ceiling, which is exactly
    /// the lend the MCP server already relies on.
    enumerations: Arc<crate::providers::InstalledEnumerations>,
    /// Fires between a derivation finishing and its result being stored.
    ///
    /// The window a test cannot otherwise reach: an `invalidate()` landing after
    /// the tick holds a finished derivation but before that derivation is
    /// written back. Nothing in production ever sets it.
    #[cfg(test)]
    #[allow(clippy::type_complexity)]
    before_store: Mutex<Option<Box<dyn Fn() + Send>>>,
    /// Counts derivations queued for the config slot's store lock.
    ///
    /// The second window a test cannot otherwise reach: a derivation that has
    /// already read the epoch and is now BLOCKED on the slot. Only a thread that
    /// can observe that state can move the epoch inside it, which is the whole
    /// of the ordering this cache claims. Nothing in production reads it.
    #[cfg(test)]
    store_gate: StoreGate,
}

/// The waiter count behind [`TickCache::store_gate`], with a condvar so a test
/// waits on the state instead of guessing how long reaching it takes.
#[cfg(test)]
#[derive(Default)]
struct StoreGate {
    waiting: Mutex<usize>,
    signal: std::sync::Condvar,
}

#[cfg(test)]
impl StoreGate {
    /// About to block on the slot.
    fn entering(&self) {
        let mut waiting = self.waiting.lock().unwrap_or_else(|e| e.into_inner());
        *waiting += 1;
        self.signal.notify_all();
    }

    /// Holding the slot.
    fn entered(&self) {
        let mut waiting = self.waiting.lock().unwrap_or_else(|e| e.into_inner());
        *waiting = waiting.saturating_sub(1);
    }

    /// Block until a derivation is queued for the slot.
    ///
    /// `timeout` is a deadlock escape, never a timing assertion — a caller
    /// asserts on the returned bool.
    fn await_waiter(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut waiting = self.waiting.lock().unwrap_or_else(|e| e.into_inner());
        while *waiting == 0 {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            let (next, _) = self
                .signal
                .wait_timeout(waiting, remaining)
                .unwrap_or_else(|e| e.into_inner());
            waiting = next;
        }
        true
    }
}

impl TickCache {
    pub(crate) fn new() -> Self {
        Self {
            epoch: AtomicU64::new(0),
            next_derivation_id: AtomicU64::new(0),
            config: Mutex::new(None),
            modules: Mutex::new(None),
            store: Mutex::new(None),
            enumerations: Arc::new(crate::providers::InstalledEnumerations::default()),
            #[cfg(test)]
            before_store: Mutex::new(None),
            #[cfg(test)]
            store_gate: StoreGate::default(),
        }
    }

    /// Run `hook` between a derivation finishing and its result being stored.
    #[cfg(test)]
    fn on_before_store(&self, hook: impl Fn() + Send + 'static) {
        *self.before_store.lock().unwrap_or_else(|e| e.into_inner()) = Some(Box::new(hook));
    }

    fn fire_before_store(&self) {
        #[cfg(test)]
        {
            // Cloned out of the lock: the hook's whole purpose is to reach back
            // into this cache, and a hook holding its own slot's guard would
            // deadlock rather than race.
            let hook = self
                .before_store
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            if let Some(hook) = hook {
                hook();
            }
        }
    }

    /// Drop every config-derived slot.
    ///
    /// Called when the file watcher sees a write under the config directory —
    /// the belt to the fingerprint's braces. The watcher sees writes the
    /// recorder cannot attribute (an editor's rename dance, a `git pull` into
    /// the config repo) and costs one re-derivation to act on.
    pub(crate) fn invalidate(&self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
        *self.config.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self.modules.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// The enumeration memo every tick's `PackageContext` borrows.
    pub(crate) fn enumerations(&self) -> &crate::providers::InstalledEnumerations {
        &self.enumerations
    }

    /// The daemon's state store, opened on the first tick that wants one and
    /// lent to every tick after it.
    ///
    /// The returned handle holds the slot's lock for as long as the tick uses
    /// the connection, which is what keeps a non-`Sync` sqlite handle sound
    /// across the blocking threads ticks run on.
    pub(crate) fn store(
        &self,
        open: impl FnOnce() -> crate::errors::Result<StateStore>,
    ) -> crate::errors::Result<StoreHandle<'_>> {
        let mut slot = self.store.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(held) = slot.as_mut() {
            match held.path_verdict() {
                HeldFileVerdict::Same => {}
                HeldFileVerdict::Gone => {
                    tracing::warn!(
                        "state database moved or was replaced — reopening the daemon's connection"
                    );
                    *slot = None;
                }
                HeldFileVerdict::Unreadable(reason) => {
                    // A probe that failed says nothing about whether the file
                    // moved, and dropping a working connection over it would
                    // trade a transient error for a lost store. Said once per
                    // streak, because a condition lasting an hour would
                    // otherwise say so on every tick.
                    if let Some(reason) = reason {
                        tracing::warn!(
                            error = %reason,
                            "cannot inspect the state database file — keeping the open connection"
                        );
                    }
                }
            }
        }
        if slot.is_none() {
            *slot = Some(HeldStore::capture(open()?));
        }
        Ok(StoreHandle { slot })
    }

    /// The config-derived objects for `identity`, deriving them only when the
    /// held ones can no longer stand.
    ///
    /// `derive` runs with NO lock held and inside a [`ConfigInputRecorder`], so
    /// what it reads is what the next tick re-stats.
    pub(crate) fn config_derivation<E>(
        &self,
        config_path: &Path,
        profile_override: Option<&str>,
        derive: impl FnOnce() -> std::result::Result<DerivedConfig, E>,
    ) -> std::result::Result<CachedConfig, E> {
        let identity = (
            config_path.to_path_buf(),
            profile_override.map(str::to_string),
        );
        // The candidate is taken out from under the lock BEFORE its inputs are
        // re-stat'd: `unchanged()` is one syscall per recorded input, and holding
        // the slot across them would pin the watcher thread's `invalidate()`
        // behind a stat storm it has nothing to do with.
        let candidate = self
            .config
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .filter(|held| held.identity == identity)
            .filter(|held| held.derived_at.elapsed() < CONFIG_REUSE_MAX_AGE)
            .map(|held| {
                (
                    Arc::clone(&held.inputs),
                    CachedConfig::from_held(held, true),
                )
            });
        if let Some((inputs, hit)) = candidate
            && inputs.unchanged()
        {
            return Ok(hit);
        }

        let started_at_epoch = self.epoch.load(Ordering::SeqCst);
        let recorder = ConfigInputRecorder::start();
        let derived = derive();
        let inputs = recorder.finish();
        let derived = derived?;

        let held = ConfigDerivation {
            identity,
            id: self.next_derivation_id.fetch_add(1, Ordering::SeqCst),
            inputs: Arc::new(inputs),
            derived_at: Instant::now(),
            cfg: Arc::new(derived.cfg),
            profile_name: derived.profile_name,
            resolved: Arc::new(derived.resolved),
            source_module_roots: Arc::new(derived.source_module_roots),
            registry: Arc::new(derived.registry),
            source_advisories: Arc::new(derived.source_advisories),
        };
        let fresh = CachedConfig::from_held(&held, false);
        self.fire_before_store();
        // The derivation ran unlocked, so an invalidation may have landed while
        // it was in flight. Storing it anyway would answer the next tick with a
        // config read before the change that invalidated it. The epoch is read
        // UNDER the slot lock, and `invalidate` bumps it BEFORE it takes that
        // lock, so there is no window between the two in which an invalidation
        // can be lost.
        #[cfg(test)]
        self.store_gate.entering();
        let mut slot = self.config.lock().unwrap_or_else(|e| e.into_inner());
        #[cfg(test)]
        self.store_gate.entered();
        if self.epoch.load(Ordering::SeqCst) == started_at_epoch {
            *slot = Some(held);
        }
        drop(slot);
        Ok(fresh)
    }

    /// The resolved modules for `config`, resolving them only when the held set
    /// can no longer stand.
    pub(crate) fn modules(
        &self,
        config: &CachedConfig,
        resolve: impl FnOnce() -> Vec<ResolvedModule>,
    ) -> Arc<Vec<ResolvedModule>> {
        let candidate = self
            .modules
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .filter(|held| held.config_derivation_id == config.derivation_id)
            .filter(|held| held.resolved_at.elapsed() < MODULE_REUSE_TTL)
            .map(|held| (Arc::clone(&held.inputs), Arc::clone(&held.modules)));
        if let Some((inputs, hit)) = candidate
            && inputs.unchanged()
        {
            return hit;
        }

        let started_at_epoch = self.epoch.load(Ordering::SeqCst);
        let recorder = ConfigInputRecorder::start();
        let modules = Arc::new(resolve());
        let inputs = Arc::new(recorder.finish());

        self.fire_before_store();
        let mut slot = self.modules.lock().unwrap_or_else(|e| e.into_inner());
        if self.epoch.load(Ordering::SeqCst) == started_at_epoch {
            *slot = Some(ModuleDerivation {
                config_derivation_id: config.derivation_id,
                inputs,
                resolved_at: Instant::now(),
                modules: Arc::clone(&modules),
            });
        }
        drop(slot);
        modules
    }
}

/// One tick's borrow of the daemon's state store.
pub(crate) struct StoreHandle<'a> {
    slot: std::sync::MutexGuard<'a, Option<HeldStore>>,
}

impl StoreHandle<'_> {
    /// The lent connection.
    ///
    /// `None` cannot happen — [`TickCache::store`] fills the slot before it
    /// mints a handle — and is reported rather than unwrapped because library
    /// code does not panic; a caller treats it as the open failure it would
    /// have to be.
    pub(crate) fn get(&self) -> Option<&StateStore> {
        self.slot.as_ref().map(|held| &held.store)
    }
}

/// A held connection and the identity of the file it was opened on.
///
/// cfgd itself MOVES the state database: `StateStore::open` performs a
/// legacy-state-dir migration that checkpoints the WAL and renames the file. A
/// CLI run doing that while the daemon holds a connection leaves the daemon
/// writing into an orphaned inode — no error, no log line, forever, where before
/// this cache the next tick's `open` recovered within one interval. The same
/// shape covers an operator deleting the state directory. This is the pattern
/// `acquire_lock_at` already uses for lock files: after taking the thing, check
/// that the path still names it.
struct HeldStore {
    store: StateStore,
    /// The file the connection is attached to, captured at open. `None` for a
    /// memory-backed connection or a path that could not be inspected, which
    /// makes the check stand down rather than reopen on every lend.
    identity: Option<crate::FileIdentity>,
    /// Whether the current run of failed probes has already been reported, so a
    /// lasting condition says so once rather than once per tick. Cleared the
    /// moment a probe succeeds again.
    unreadable_reported: bool,
}

/// What the path a held connection was opened on names now.
enum HeldFileVerdict {
    /// Still the same file — or the question does not apply.
    Same,
    /// The path names nothing, or names a different file.
    Gone,
    /// The probe itself failed, carrying the reason the first time in a streak.
    Unreadable(Option<std::io::Error>),
}

impl HeldStore {
    fn capture(store: StateStore) -> Self {
        let identity = store.db_path().and_then(crate::file_identity);
        Self {
            store,
            identity,
            unreadable_reported: false,
        }
    }

    /// What the path this connection was opened on names now.
    ///
    /// A failed probe is deliberately NOT a mismatch: a directory that lost `+x`
    /// or a third party holding the file open on Windows would otherwise close a
    /// working connection over a question that was never answered.
    fn path_verdict(&mut self) -> HeldFileVerdict {
        let Some(captured) = self.identity else {
            return HeldFileVerdict::Same;
        };
        let Some(path) = self.store.db_path() else {
            return HeldFileVerdict::Gone;
        };
        match crate::try_file_identity(path) {
            Ok(current) => {
                self.unreadable_reported = false;
                if current == captured {
                    HeldFileVerdict::Same
                } else {
                    HeldFileVerdict::Gone
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => HeldFileVerdict::Gone,
            Err(err) => {
                let first = !self.unreadable_reported;
                self.unreadable_reported = true;
                HeldFileVerdict::Unreadable(first.then_some(err))
            }
        }
    }
}

/// What a config derivation produces, before it is shared.
pub(crate) struct DerivedConfig {
    pub(crate) cfg: CfgdConfig,
    pub(crate) profile_name: String,
    pub(crate) resolved: ResolvedProfile,
    pub(crate) source_module_roots: Vec<SourceModuleRoot>,
    pub(crate) registry: ProviderRegistry,
    /// What the composition said out loud about sources it skipped.
    pub(crate) source_advisories: Vec<SourceAdvisory>,
}

/// One tick's handle on the config-derived objects.
#[derive(Clone)]
pub(crate) struct CachedConfig {
    pub(crate) cfg: Arc<CfgdConfig>,
    pub(crate) profile_name: String,
    pub(crate) resolved: Arc<ResolvedProfile>,
    pub(crate) source_module_roots: Arc<Vec<SourceModuleRoot>>,
    pub(crate) registry: Arc<ProviderRegistry>,
    /// The composition's skip advisories, carried so a REUSING tick can re-state
    /// a condition that still holds. See [`Self::advisories_to_restate`].
    source_advisories: Arc<Vec<SourceAdvisory>>,
    /// Whether this handle came from a held derivation rather than from one this
    /// caller just ran. The derivation printed its own advisories; a reuse did
    /// not, and has to.
    reused: bool,
    /// Which derivation this came from, so a module set can say which config it
    /// was resolved against.
    derivation_id: u64,
}

impl CachedConfig {
    fn from_held(held: &ConfigDerivation, reused: bool) -> Self {
        Self {
            cfg: Arc::clone(&held.cfg),
            profile_name: held.profile_name.clone(),
            resolved: Arc::clone(&held.resolved),
            source_module_roots: Arc::clone(&held.source_module_roots),
            registry: Arc::clone(&held.registry),
            source_advisories: Arc::clone(&held.source_advisories),
            reused,
            derivation_id: held.id,
        }
    }

    /// The advisories this tick still owes the operator.
    ///
    /// Empty on the tick that derived — the composition printed them itself as
    /// it went. Non-empty on every tick that REUSED that derivation, so a source
    /// with no local cache is reported once per tick exactly as it was before
    /// the derivation was ever held across ticks. Suppressing them would make an
    /// unchanged, still-broken source look like one that got fixed.
    pub(crate) fn advisories_to_restate(&self) -> &[SourceAdvisory] {
        if self.reused {
            &self.source_advisories
        } else {
            &[]
        }
    }
}

/// A derivation whose `profile_name` says which call produced it, so a test can
/// tell a reused object from a re-derived one without comparing addresses.
///
/// Shared with the daemon's own tests, which drive a tick against this cache.
#[cfg(test)]
pub(super) fn test_derived_config(
    marker: &str,
    source_advisories: Vec<SourceAdvisory>,
) -> DerivedConfig {
    use crate::config::{ConfigMetadata, ConfigSpec, MergedProfile};
    DerivedConfig {
        cfg: CfgdConfig {
            api_version: crate::API_VERSION.into(),
            kind: "CfgdConfig".into(),
            metadata: ConfigMetadata {
                name: marker.into(),
            },
            spec: ConfigSpec::default(),
            deprecations: Vec::new(),
        },
        profile_name: marker.to_string(),
        resolved: ResolvedProfile {
            layers: Vec::new(),
            merged: MergedProfile::default(),
        },
        source_module_roots: Vec::new(),
        registry: ProviderRegistry::new(),
        source_advisories,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn derived(marker: &str) -> DerivedConfig {
        test_derived_config(marker, Vec::new())
    }

    /// The same, carrying what the composition said about skipped sources.
    fn derived_advising(marker: &str, source_advisories: Vec<SourceAdvisory>) -> DerivedConfig {
        test_derived_config(marker, source_advisories)
    }

    /// Derive against `input`, recording it the way a real reader does.
    fn derive_reading(
        cache: &TickCache,
        config_path: &Path,
        input: &Path,
        calls: &std::cell::Cell<usize>,
        marker: &str,
    ) -> CachedConfig {
        let held = cache.config_derivation(config_path, None, || {
            calls.set(calls.get() + 1);
            crate::record_config_input(input);
            Ok::<_, ()>(derived(marker))
        });
        match held {
            Ok(c) => c,
            Err(()) => unreachable!("the derivation above cannot fail"),
        }
    }

    #[test]
    fn an_unchanged_input_reuses_the_whole_derivation() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(&config_path, "first").unwrap();
        let cache = TickCache::new();
        let calls = std::cell::Cell::new(0);

        let first = derive_reading(&cache, &config_path, &config_path, &calls, "first");
        let second = derive_reading(&cache, &config_path, &config_path, &calls, "second");

        assert_eq!(calls.get(), 1, "the second ask must not have re-derived");
        assert_eq!(second.profile_name, "first");
        assert_eq!(first.derivation_id, second.derivation_id);
    }

    #[test]
    fn a_changed_input_re_derives() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(&config_path, "first").unwrap();
        let cache = TickCache::new();
        let calls = std::cell::Cell::new(0);

        derive_reading(&cache, &config_path, &config_path, &calls, "first");
        // A different LENGTH, so the fingerprint moves whatever the filesystem's
        // timestamp granularity is.
        std::fs::write(&config_path, "second — longer than the first").unwrap();
        let second = derive_reading(&cache, &config_path, &config_path, &calls, "second");

        assert_eq!(calls.get(), 2);
        assert_eq!(second.profile_name, "second");
    }

    #[test]
    fn a_changed_input_that_is_not_the_config_file_re_derives() {
        // The gate is on what the derivation READ, not on the file the daemon
        // was pointed at: a profile, a source manifest and a module document
        // are all inputs, and a config whose own bytes never move is the normal
        // case for every one of them.
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(&config_path, "unchanging").unwrap();
        let profile_path = tmp.path().join("profiles.yaml");
        std::fs::write(&profile_path, "first").unwrap();
        let cache = TickCache::new();
        let calls = std::cell::Cell::new(0);

        derive_reading(&cache, &config_path, &profile_path, &calls, "first");
        std::fs::write(&profile_path, "second — longer than the first").unwrap();
        derive_reading(&cache, &config_path, &profile_path, &calls, "second");

        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn a_derivation_that_recorded_nothing_is_never_reused() {
        // A derivation with no inputs is one whose readers all went unreported,
        // and there is nothing to re-stat that could say it went stale. Reusing
        // it would be reuse forever.
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        let cache = TickCache::new();
        let mut calls = 0;
        for _ in 0..2 {
            let _ = cache.config_derivation(&config_path, None, || {
                calls += 1;
                Ok::<_, ()>(derived("marker"))
            });
        }
        assert_eq!(calls, 2);
    }

    #[test]
    fn an_invalidation_racing_a_derivation_is_not_lost() {
        // The derivation runs unlocked, so a watcher event can land while it is
        // in flight. Storing it anyway would answer the NEXT tick with a config
        // read before the change that invalidated it.
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(&config_path, "first").unwrap();
        let cache = TickCache::new();
        let calls = std::cell::Cell::new(0);

        let raced = cache.config_derivation(&config_path, None, || {
            calls.set(calls.get() + 1);
            crate::record_config_input(&config_path);
            cache.invalidate();
            Ok::<_, ()>(derived("raced"))
        });
        assert!(raced.is_ok());
        derive_reading(&cache, &config_path, &config_path, &calls, "after");

        assert_eq!(calls.get(), 2, "the raced derivation must not have stood");
    }

    #[test]
    fn a_different_identity_re_derives() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(&config_path, "first").unwrap();
        let cache = TickCache::new();
        let calls = std::cell::Cell::new(0);

        derive_reading(&cache, &config_path, &config_path, &calls, "first");
        let held = cache.config_derivation(&config_path, Some("other-profile"), || {
            calls.set(calls.get() + 1);
            crate::record_config_input(&config_path);
            Ok::<_, ()>(derived("other"))
        });
        assert!(held.is_ok());

        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn modules_stand_while_their_config_derivation_does() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(&config_path, "first").unwrap();
        let module_path = tmp.path().join("module.yaml");
        std::fs::write(&module_path, "first").unwrap();
        let cache = TickCache::new();
        let calls = std::cell::Cell::new(0);
        let resolves = std::cell::Cell::new(0);

        let resolve = |cache: &TickCache, held: &CachedConfig| {
            cache.modules(held, || {
                resolves.set(resolves.get() + 1);
                crate::record_config_input(&module_path);
                Vec::new()
            })
        };

        let first = derive_reading(&cache, &config_path, &config_path, &calls, "first");
        resolve(&cache, &first);
        let second = derive_reading(&cache, &config_path, &config_path, &calls, "second");
        resolve(&cache, &second);
        assert_eq!(resolves.get(), 1, "an unchanged config re-resolves nothing");

        // A moved module document re-resolves even though the config did not.
        std::fs::write(&module_path, "second — longer than the first").unwrap();
        let third = derive_reading(&cache, &config_path, &config_path, &calls, "third");
        resolve(&cache, &third);
        assert_eq!(resolves.get(), 2);
    }

    #[test]
    fn a_re_derived_config_re_resolves_its_modules() {
        // The module set describes ONE config derivation. A config that moved
        // may name different modules, so the module inputs standing still says
        // nothing about whether the set is right.
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(&config_path, "first").unwrap();
        let module_path = tmp.path().join("module.yaml");
        std::fs::write(&module_path, "unchanging").unwrap();
        let cache = TickCache::new();
        let calls = std::cell::Cell::new(0);
        let resolves = std::cell::Cell::new(0);

        let first = derive_reading(&cache, &config_path, &config_path, &calls, "first");
        cache.modules(&first, || {
            resolves.set(resolves.get() + 1);
            crate::record_config_input(&module_path);
            Vec::new()
        });

        std::fs::write(&config_path, "second — longer than the first").unwrap();
        let second = derive_reading(&cache, &config_path, &config_path, &calls, "second");
        cache.modules(&second, || {
            resolves.set(resolves.get() + 1);
            crate::record_config_input(&module_path);
            Vec::new()
        });

        assert_eq!(calls.get(), 2);
        assert_eq!(resolves.get(), 2);
    }

    #[test]
    fn the_store_is_opened_once_and_lent_after() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let cache = TickCache::new();
        let mut opens = 0;
        for _ in 0..3 {
            let held = cache
                .store(|| {
                    opens += 1;
                    StateStore::open_in_dir(&state_dir)
                })
                .unwrap();
            assert!(held.get().is_some());
        }
        assert_eq!(opens, 1);
    }

    #[test]
    fn an_epoch_that_moves_while_the_store_is_blocked_discards_the_derivation() {
        // The ordering the epoch-under-the-lock rule exists for, and the only
        // one that tells it apart from reading the epoch first: the derivation
        // has finished, it is BLOCKED on the slot, and the epoch moves while it
        // waits. A derivation that read the epoch before queueing has already
        // decided to store by then; one that reads it under the lock sees the
        // bump the handover published.
        //
        // Every step waits on an observable — the hook's channel, the store
        // gate's waiter count — so no ordering here rests on how long anything
        // took.
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(&config_path, "first").unwrap();
        let cache = TickCache::new();
        let calls = std::cell::Cell::new(0);

        let (take_tx, take_rx) = std::sync::mpsc::channel::<()>();
        let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
        // The hook runs on the deriving thread and returns only once the slot is
        // held elsewhere, so the derivation is guaranteed to block below.
        cache.on_before_store(move || {
            take_tx.send(()).unwrap();
            held_rx.recv().unwrap();
        });

        let holder = &cache;
        std::thread::scope(|scope| {
            scope.spawn(move || {
                take_rx.recv().unwrap();
                let slot = holder.config.lock().unwrap_or_else(|e| e.into_inner());
                held_tx.send(()).unwrap();
                assert!(
                    holder.store_gate.await_waiter(GATE_BUDGET),
                    "the derivation never queued for the slot"
                );
                // The epoch alone: `invalidate` would block on the very lock
                // this thread is holding.
                holder.epoch.fetch_add(1, Ordering::SeqCst);
                drop(slot);
            });

            derive_reading(&cache, &config_path, &config_path, &calls, "raced");
        });

        assert_eq!(calls.get(), 1);
        derive_reading(&cache, &config_path, &config_path, &calls, "after");
        assert_eq!(
            calls.get(),
            2,
            "a derivation the epoch outran must not have been stored"
        );

        // And an undisturbed derivation still stands, so the discard is aimed at
        // the moved epoch rather than at every store.
        derive_reading(&cache, &config_path, &config_path, &calls, "third");
        assert_eq!(calls.get(), 2);
    }

    /// Deadlock escape for the store-gate wait. Asserted on as a bool, never as
    /// a measurement.
    const GATE_BUDGET: Duration = Duration::from_secs(10);

    #[test]
    fn every_reusing_tick_restates_the_composition_advisories() {
        // A source with no local cache is skipped, and the composition says so
        // once per tick. Holding the derivation across ticks must not turn that
        // into "said once, ever" — an unchanged, still-broken source would then
        // look like one that got fixed.
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(&config_path, "first").unwrap();
        let cache = TickCache::new();
        let advisory = SourceAdvisory::skipped("source 'team' has never been synced — skipping");

        // Asserted per tick, so a shortfall names the tick that produced it and
        // says which of the two ways it went wrong: a tick that re-derived
        // (derivations moved) is a reuse failure, and a tick that reused
        // without restating (restated did not move) is a restatement failure.
        // A total alone cannot tell those apart.
        let mut derivations = 0;
        let mut restated = 0;
        for tick in 1..=4 {
            let held = cache.config_derivation(&config_path, None, || {
                derivations += 1;
                crate::record_config_input(&config_path);
                Ok::<_, ()>(derived_advising("first", vec![advisory.clone()]))
            });
            let held = match held {
                Ok(c) => c,
                Err(()) => unreachable!("the derivation above cannot fail"),
            };
            restated += held.advisories_to_restate().len();
            assert_eq!(
                derivations, 1,
                "tick {tick} re-derived the composition — the held derivation \
                 was not reused"
            );
            assert_eq!(
                restated,
                tick - 1,
                "tick {tick} reused the derivation and owed the operator the \
                 advisory it did not restate"
            );
        }

        assert_eq!(derivations, 1, "only the first tick composed");
        assert_eq!(
            restated, 3,
            "each of the three reusing ticks owes the operator the advisory"
        );
    }

    #[test]
    fn a_reusing_tick_restates_a_bypass_where_a_quiet_daemon_hears_it() {
        // The reason the advisory carries its channel: a daemon ticks at
        // Verbosity::Quiet, where a Role::Warn status is dropped. A bypass of a
        // security constraint the user declared has to reach the operator on
        // every reusing tick, not only on the one that composed.
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(&config_path, "first").unwrap();
        let cache = TickCache::new();
        let advisory = SourceAdvisory::bypassed(
            "source 'team': --allow-unsigned bypassed requireSignedCommits",
        );

        let derive = || {
            let held = cache.config_derivation(&config_path, None, || {
                crate::record_config_input(&config_path);
                Ok::<_, ()>(derived_advising("first", vec![advisory.clone()]))
            });
            match held {
                Ok(c) => c,
                Err(()) => unreachable!("the derivation above cannot fail"),
            }
        };

        drop(derive());
        let reusing = derive();

        let (printer, buf) = crate::output::Printer::for_test_at(crate::output::Verbosity::Quiet);
        for advisory in reusing.advisories_to_restate() {
            advisory.restate(&printer);
        }
        printer.flush();

        let out = crate::test_helpers::captured_text(&buf);
        assert!(
            out.contains("bypassed requireSignedCommits"),
            "a quiet daemon tick must still hear the bypass, got: {out:?}"
        );
    }

    #[test]
    fn a_state_database_that_moved_out_from_under_the_daemon_is_reopened() {
        // cfgd itself moves this file: `StateStore::open` migrates a legacy
        // state dir by renaming the database. A daemon holding the old
        // connection would write into an orphaned inode with no error, forever.
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join("state");
        let cache = TickCache::new();
        let mut opens = 0;
        let open_once = |cache: &TickCache, opens: &mut usize| {
            let held = cache
                .store(|| {
                    *opens += 1;
                    StateStore::open_in_dir(&state_dir)
                })
                .unwrap();
            assert!(held.get().is_some());
        };

        open_once(&cache, &mut opens);
        open_once(&cache, &mut opens);
        assert_eq!(opens, 1, "an unmoved database is lent, not reopened");

        std::fs::remove_dir_all(&state_dir).unwrap();
        open_once(&cache, &mut opens);
        assert_eq!(
            opens, 2,
            "the path no longer names the file that was opened"
        );

        open_once(&cache, &mut opens);
        assert_eq!(opens, 2, "the reopened connection is lent like any other");
    }
}
