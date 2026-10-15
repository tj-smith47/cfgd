use std::borrow::Borrow;
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use cfgd_core::config::{CfgdConfig, LayerSources, PackagesSpec, ResolvedProfile};
use cfgd_core::output::Printer;
use cfgd_core::providers::ProviderRegistry;
use cfgd_core::state::StateStore;

use super::helpers::{config_dir, resolve_profile_for};
use super::registry::{build_registry, open_state_store};
use super::startup::StartupDocument;
use super::{Cli, packages};
use crate::packages::ManifestCache;

/// The objects one invocation builds at most once, however many of its phases
/// ask for them.
///
/// A single `cfgd status` used to parse `cfgd.yaml` twice, open the SQLite state
/// store twice and build the provider registry twice, because each half of the
/// command reached for what it needed independently. Every slot here is filled
/// on first ask and reused afterwards, so the cost of an object is paid by the
/// run that wants it and paid once — a command that never asks for the state
/// store still never opens one.
///
/// The config is the invocation's [`StartupDocument`], read and parsed before
/// dispatch; reading it through the context parses nothing.
///
/// Scoped to ONE run: `execute` builds the context and hands it to the verb,
/// which drops it when it returns. Construction is pure (it copies three
/// references and derives the config directory), so a daemon tick can hold one
/// per tick without paying for slots that tick does not use, and nothing here
/// can outlive the config it describes.
///
/// Not `Sync` by construction — the cells are single-threaded. Concurrent phases
/// receive the resolved objects (`&StateStore`, `&ProviderRegistry`), never the
/// context.
pub struct RunContext<'a> {
    cli: &'a Cli,
    printer: &'a Printer,
    config_dir: PathBuf,
    /// `cli.config` as parsed, with its deprecation notices still intact.
    startup: &'a StartupDocument,
    /// Whether those notices have already been surfaced. The drain is once per
    /// run and belongs to the first caller that reads the config for real —
    /// a command that only wants the active profile NAME must not print them,
    /// which is why the parse and the drain are separate steps.
    deprecations_drained: Cell<bool>,
    profile: OnceCell<(String, ResolvedProfile)>,
    state: OnceCell<StateStore>,
    /// The installed-state memo every [`Self::package_context`] lends.
    enumerations: cfgd_core::providers::InstalledEnumerations,
    base_registry: OnceCell<ProviderRegistry>,
    manifests: ManifestCache,
    /// Whether this run FETCHES the sources it composes, which decides
    /// [`Self::announce_cache_skips`].
    fetching_sources: Cell<bool>,
    /// Set by [`Self::hold_store_while_homeless`] until the home check over
    /// the resolved desired state says the store may open.
    store_held: Cell<bool>,
    /// The recorded state a package query reads while the store is held.
    held_view: OnceCell<HeldView>,
    /// Source conflicts composed while the store was held, recorded once it
    /// opens.
    held_conflicts: RefCell<Vec<cfgd_core::composition::ConflictResolution>>,
}

/// The store a held run reads, and why an existing one could not be read.
struct HeldView {
    records: RecordNothing<StateStore>,
    /// The read-only open's error for a `state.db` that exists. The home
    /// check cannot count what such a store recorded, so the run refuses
    /// with it before any writable open.
    unreadable: Option<String>,
}

impl HeldView {
    /// The `state.db` under the run's state directory opened read-only, or
    /// nothing when there is none (or no directory resolves).
    fn read(state_dir: Option<&Path>, scope: cfgd_core::Scope) -> Self {
        let db = super::helpers::run_state_dir(state_dir, scope)
            .ok()
            .map(|dir| dir.join(cfgd_core::state::STATE_DB_FILENAME))
            .filter(|db| db.exists());
        match db.map(|db| (StateStore::open_read_only(&db), db)) {
            None => Self {
                records: RecordNothing(None),
                unreadable: None,
            },
            Some((Ok(store), _)) => Self {
                records: RecordNothing(Some(store)),
                unreadable: None,
            },
            Some((Err(e), db)) => Self {
                records: RecordNothing(None),
                unreadable: Some(format!(
                    "cannot read {} without writing to it: {e}",
                    db.display() // native-ok: an error message, read by people
                )),
            },
        }
    }

    /// The refusal for an existing `state.db` this view could not read
    /// without writing to it.
    fn readable(&self) -> anyhow::Result<()> {
        match &self.unreadable {
            Some(message) => Err(cfgd_core::errors::CfgdError::from(
                cfgd_core::errors::StateError::Database(message.clone()),
            )
            .into()),
            None => Ok(()),
        }
    }
}

/// The state store a verb with no [`RunContext`] keeps closed while it
/// resolves with no home: the hold [`RunContext::hold_store_while_homeless`]
/// takes, for `cfgd init`, which runs before its config exists.
///
/// With a home the store opens at once and the hold is inert. With none,
/// package queries read an existing `state.db` read-only and record nothing,
/// and [`Self::open_for`] opens the store only once the resolved profile and
/// modules ask nothing a missing home refuses.
pub(in crate::cli) struct DetachedStoreHold<'d> {
    state_dir: Option<&'d Path>,
    scope: cfgd_core::Scope,
    store: DetachedStore,
}

enum DetachedStore {
    Open(StateStore),
    Held {
        view: HeldView,
        opened: OnceCell<StateStore>,
    },
}

impl<'d> DetachedStoreHold<'d> {
    /// Hold the store under `state_dir` / `scope` when no home resolves, and
    /// open it now otherwise.
    ///
    /// # Errors
    ///
    /// The store failing to open for a run with a home.
    pub(in crate::cli) fn new(
        state_dir: Option<&'d Path>,
        scope: cfgd_core::Scope,
    ) -> anyhow::Result<Self> {
        // home-optional-ok: the hold exists for the run with no home
        let store = if cfgd_core::home_dir().is_err() {
            DetachedStore::Held {
                view: HeldView::read(state_dir, scope),
                opened: OnceCell::new(),
            }
        } else {
            DetachedStore::Open(open_state_store(state_dir, scope)?)
        };
        Ok(Self {
            state_dir,
            scope,
            store,
        })
    }

    /// The store a package query reads while the run resolves.
    pub(in crate::cli) fn package_state(&self) -> &dyn cfgd_core::providers::PackageStateStore {
        match &self.store {
            DetachedStore::Open(state) => state,
            DetachedStore::Held { view, .. } => &view.records,
        }
    }

    /// The run's state store once `profile` and `modules` resolved and ask
    /// nothing a missing home refuses.
    ///
    /// # Errors
    ///
    /// The home refusal, a `state.db` the hold could not read without
    /// writing to it, or the store failing to open.
    pub(in crate::cli) fn open_for(
        &self,
        cfg: &CfgdConfig,
        profile: &cfgd_core::config::MergedProfile,
        modules: &[cfgd_core::modules::ResolvedModule],
    ) -> anyhow::Result<&StateStore> {
        let (view, opened) = match &self.store {
            DetachedStore::Open(state) => return Ok(state),
            DetachedStore::Held { view, opened } => (view, opened),
        };
        if let Some(state) = opened.get() {
            return Ok(state);
        }
        view.readable()?;
        if let Some(refusal) = super::helpers::unresolved_home_after_resolution(
            cfg,
            profile,
            modules,
            view.records.0.as_ref(),
        ) {
            return Err(cfgd_core::errors::CfgdError::from(refusal).into());
        }
        let state = open_state_store(self.state_dir, self.scope)?;
        Ok(opened.get_or_init(|| state))
    }
}

/// The state a package query reads when nothing may be recorded: what the
/// store holds, when there is one, and nothing a query would record in return.
pub(in crate::cli) struct RecordNothing<S>(pub Option<S>);

impl<S: Borrow<StateStore>> cfgd_core::providers::PackageStateStore for RecordNothing<S> {
    fn resolved_prefix(&self, manager: &str) -> cfgd_core::errors::Result<Option<(String, bool)>> {
        match &self.0 {
            Some(store) => {
                cfgd_core::providers::PackageStateStore::resolved_prefix(store.borrow(), manager)
            }
            None => Ok(None),
        }
    }

    fn record_resolved_prefix(
        &self,
        _manager: &str,
        _prefix: &str,
        _is_fallback: bool,
    ) -> cfgd_core::errors::Result<()> {
        Ok(())
    }
}

/// The state store held closed by [`RunContext::hold_store_while_homeless`].
///
/// Dropped without [`Self::open`], the hold lifts and the conflicts it kept
/// are discarded, so a run refused before its store opens leaves no
/// `state.db` behind.
pub(in crate::cli) struct StoreHold<'r, 'a> {
    run: &'r RunContext<'a>,
}

impl<'r, 'a> StoreHold<'r, 'a> {
    /// The recorded state a run reads before it may write: the store on disk
    /// opened read-only while the hold is on, and the run's own store once
    /// nothing is held. `None` is a held run with no `state.db`.
    ///
    /// # Errors
    ///
    /// A `state.db` the hold cannot read without writing to it.
    pub(in crate::cli) fn recorded_state(&self) -> anyhow::Result<Option<&StateStore>> {
        if !self.run.store_held.get() {
            return self.run.state().map(Some);
        }
        self.run.held_readable()?;
        Ok(self.run.held_store())
    }

    /// Open the run's state store once `desired` resolved and asks nothing a
    /// missing home refuses, recording the source conflicts its composition
    /// kept.
    ///
    /// A resolution that failed for any reason but the missing home still
    /// records those conflicts, as a run with a home records them while it
    /// composes, before the failure.
    pub(in crate::cli) fn open(
        self,
        cfg: &CfgdConfig,
        desired: anyhow::Result<super::helpers::DesiredState>,
    ) -> anyhow::Result<(super::helpers::DesiredState, &'r StateStore)> {
        let run = self.run;
        let desired = match desired {
            Ok(desired) => desired,
            Err(e) => {
                let conflicts = run.held_conflicts.take();
                if !conflicts.is_empty() && !is_unresolved_home(&e) {
                    drop(self);
                    if let Some(state) = run.state_opt() {
                        record_conflicts(state, &conflicts);
                    }
                }
                return Err(e);
            }
        };
        let state = self.open_for(cfg, &desired.resolved.merged, &desired.modules)?;
        Ok((desired, state))
    }

    /// Open the run's state store once `profile` and the `modules` a verb
    /// resolved ask nothing a missing home refuses, recording the source
    /// conflicts its composition kept. [`Self::open`] over a verb that
    /// resolves no whole desired state (`module create --apply`).
    ///
    /// # Errors
    ///
    /// The home refusal, a `state.db` the hold could not read without
    /// writing to it, or the store failing to open.
    pub(in crate::cli) fn open_for(
        self,
        cfg: &CfgdConfig,
        profile: &cfgd_core::config::MergedProfile,
        modules: &[cfgd_core::modules::ResolvedModule],
    ) -> anyhow::Result<&'r StateStore> {
        let run = self.run;
        run.held_readable()?;
        if let Some(refusal) = super::helpers::unresolved_home_after_resolution(
            cfg,
            profile,
            modules,
            run.held_store(),
        ) {
            return Err(cfgd_core::errors::CfgdError::from(refusal).into());
        }
        self.release()
    }

    /// Open the run's state store once the verb has put every question it has
    /// to the home, recording the source conflicts its composition kept. A
    /// verb whose home work is narrower than its whole desired state (one
    /// module's env, a backup unit's paths, a compliance check's targets) asks
    /// its own questions and releases the hold here, where
    /// [`Self::open`] asks the whole resolution's.
    ///
    /// # Errors
    ///
    /// A `state.db` the hold could not read without writing to it, or the
    /// store failing to open.
    pub(in crate::cli) fn release(self) -> anyhow::Result<&'r StateStore> {
        let run = self.run;
        run.held_readable()?;
        drop(self);
        let state = run.state()?;
        record_conflicts(state, &run.held_conflicts.take());
        Ok(state)
    }
}

/// Whether `e` is a refusal for want of a home directory.
pub(in crate::cli) fn is_unresolved_home(e: &anyhow::Error) -> bool {
    use cfgd_core::errors::{CfgdError, ConfigError, StateError};
    let home_state = |s: &StateError| {
        matches!(
            s,
            StateError::HomeDirectoryUnresolved { .. }
                | StateError::HomeUnset
                | StateError::HomeUnresolved { .. }
                | StateError::EnvHomeUnresolved { .. }
        )
    };
    let home_config = |c: &ConfigError| matches!(c, ConfigError::HomeUnresolved { .. });
    // Every link is asked: a refusal reached through module resolution arrives
    // wrapped in an error of another kind.
    e.chain()
        .any(|cause| match cause.downcast_ref::<CfgdError>() {
            Some(CfgdError::State(s)) => home_state(s),
            Some(CfgdError::Config(c)) => home_config(c),
            Some(_) => false,
            None => {
                cause.downcast_ref::<StateError>().is_some_and(home_state)
                    || cause.downcast_ref::<ConfigError>().is_some_and(home_config)
            }
        })
}

impl Drop for StoreHold<'_, '_> {
    fn drop(&mut self) {
        self.run.store_held.set(false);
    }
}

/// Best effort: a conflict the store refuses is logged, so a read-only store
/// never blocks a composition.
fn record_conflicts(state: &StateStore, conflicts: &[cfgd_core::composition::ConflictResolution]) {
    for conflict in conflicts {
        if let Err(e) = state.record_source_conflict(
            &conflict.winning_source,
            "composition",
            &conflict.resource_id,
            conflict.resolution_type.label(),
            Some(&conflict.details),
        ) {
            tracing::warn!(
                error = %e,
                winning_source = %conflict.winning_source,
                resource_id = %conflict.resource_id,
                "failed to persist source conflict to state store; conflict history may be incomplete",
            );
        }
    }
}

impl<'a> RunContext<'a> {
    pub(in crate::cli) fn new(
        cli: &'a Cli,
        printer: &'a Printer,
        startup: &'a StartupDocument,
    ) -> Self {
        Self {
            cli,
            printer,
            config_dir: config_dir(cli),
            startup,
            deprecations_drained: Cell::new(false),
            profile: OnceCell::new(),
            state: OnceCell::new(),
            enumerations: cfgd_core::providers::InstalledEnumerations::default(),
            base_registry: OnceCell::new(),
            manifests: ManifestCache::default(),
            fetching_sources: Cell::new(false),
            store_held: Cell::new(false),
            held_view: OnceCell::new(),
            held_conflicts: RefCell::new(Vec::new()),
        }
    }

    /// Declare that this run fetches the sources it composes, so the
    /// composition stops advising the reader to run the command they are
    /// running.
    ///
    /// The two advisories a cached load can raise — a source with no checkout
    /// yet, and one cloned from an origin the spec no longer names — both end
    /// in "run `cfgd sync`". Emitted by `cfgd sync` itself, three lines above
    /// the section that does the fetching, they name a remedy already in
    /// progress. Nothing else the composition says is touched: a constraint
    /// violation, a conflict preview and the `allowScripts` disclosure are all
    /// facts the fetching verb is the right place to hear.
    pub(in crate::cli) fn fetching_sources(&self) {
        self.fetching_sources.set(true);
    }

    /// Whether a source-cache skip should be announced on this run.
    pub(in crate::cli) fn announce_cache_skips(&self) -> bool {
        !self.fetching_sources.get()
    }

    pub(in crate::cli) fn cli(&self) -> &'a Cli {
        self.cli
    }

    pub(in crate::cli) fn printer(&self) -> &'a Printer {
        self.printer
    }

    /// The document this run reads its config from, for the verbs that also
    /// need its path or the text it was parsed from.
    pub(in crate::cli) fn startup(&self) -> &'a StartupDocument {
        self.startup
    }

    /// The directory holding `cli.config`, which relative source paths and
    /// manifest references resolve against.
    pub(in crate::cli) fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    /// The run's config WITHOUT surfacing its deprecation notices. Only the
    /// callers that need nothing but a name off the config (the active profile
    /// a module-only run stamps into `CFGD_PROFILE`), and the best-effort reads
    /// of a verb that reports on something other than the config (the daemon's
    /// status), read through here.
    ///
    /// Every read reports the document as a config input, as a read from disk
    /// would: a saved plan records what its derivation read, and the startup
    /// read happened before the plan opened its recorder.
    pub(in crate::cli) fn config_unannounced(&self) -> cfgd_core::errors::Result<&'a CfgdConfig> {
        cfgd_core::record_config_input(self.startup.path());
        self.startup.config_result()
    }

    /// The run's config, with its deprecation notices surfaced exactly once.
    pub(in crate::cli) fn config(&self) -> cfgd_core::errors::Result<&'a CfgdConfig> {
        let cfg = self.config_unannounced()?;
        if !self.deprecations_drained.replace(true) {
            for msg in &cfg.deprecations {
                self.printer.deprecation(msg);
            }
        }
        Ok(cfg)
    }

    /// The run's config, the name of the profile in force, and that profile's
    /// resolution, resolved at most once per run.
    pub(in crate::cli) fn config_and_profile(
        &self,
    ) -> anyhow::Result<(&CfgdConfig, &str, &ResolvedProfile)> {
        let cfg = self.config()?;
        if let Some((name, resolved)) = self.profile.get() {
            return Ok((cfg, name, resolved));
        }
        let pair = resolve_profile_for(self.cli, cfg)?;
        let (name, resolved) = self.profile.get_or_init(|| pair);
        Ok((cfg, name, resolved))
    }

    /// Every env var name a declared secret exports, across the profile chain
    /// this run resolves — the set `MaskEnvValues::Secrets` masks by.
    ///
    /// `None` where the run could not resolve a chain at all, which a caller
    /// reads as "say nothing", never as "no secrets": a `Secrets` run holding
    /// no set masks every value rather than printing one it cannot vouch for.
    pub(in crate::cli) fn secret_env_names(&self) -> Option<BTreeSet<String>> {
        self.config_and_profile()
            .ok()
            .map(|(_, _, resolved)| resolved.secret_env_names())
    }

    /// [`super::helpers::active_profile_name`] over the run's config, read
    /// from the startup document.
    pub(in crate::cli) fn active_profile_name(&self) -> String {
        super::helpers::active_profile_name(self.cli, self.config_unannounced().ok())
    }

    /// The run's state store, opened at most once.
    ///
    /// The cache root resolves first, through the resolver every module and
    /// source read takes, so a run that cannot place its cache refuses before
    /// the open creates a `state.db` it would leave behind.
    pub(in crate::cli) fn state(&self) -> anyhow::Result<&StateStore> {
        if let Some(state) = self.state.get() {
            return Ok(state);
        }
        debug_assert!(
            !self.store_held.get(),
            "the state store opened while a StoreHold kept it closed"
        );
        super::helpers::module_cache_dir(self.cli)?;
        let state = open_state_store(self.cli.state_dir.as_deref(), self.cli.scope())?;
        Ok(self.state.get_or_init(|| state))
    }

    /// The run's state store, or `None` when it cannot be opened — for the
    /// advisory paths that record if they can and carry on if they cannot.
    pub(in crate::cli) fn state_opt(&self) -> Option<&StateStore> {
        self.state().ok()
    }

    /// Keep the state store closed while this run resolves its desired state,
    /// when no home directory resolves and the store is not open yet.
    ///
    /// With no home, whether the run may write at all turns on the `~/` work
    /// the resolved profile, sources and modules declare, which only the
    /// resolution knows; [`StoreHold::open`] asks that question and only then
    /// opens the store. Meanwhile a package query reads the store read-only
    /// and records nothing, and a source conflict waits to be recorded. With a
    /// home the check refuses nothing, so the hold is inert.
    pub(in crate::cli) fn hold_store_while_homeless(&self) -> StoreHold<'_, 'a> {
        // home-optional-ok: the hold exists for the run with no home
        let homeless = cfgd_core::home_dir().is_err();
        self.store_held.set(homeless && self.state.get().is_none());
        StoreHold { run: self }
    }

    /// Record `conflicts` in the run's state store, or keep them for
    /// [`StoreHold::open`] while the store is held.
    pub(in crate::cli) fn record_source_conflicts(
        &self,
        conflicts: &[cfgd_core::composition::ConflictResolution],
    ) {
        if self.store_held.get() {
            self.held_conflicts
                .borrow_mut()
                .extend(conflicts.iter().cloned());
        } else if let Some(state) = self.state_opt() {
            record_conflicts(state, conflicts);
        }
    }

    /// The recorded state while the store is held: an existing `state.db`
    /// opened read-only, which neither migrates it nor creates a file.
    fn held_view(&self) -> &HeldView {
        self.held_view
            .get_or_init(|| HeldView::read(self.cli.state_dir.as_deref(), self.cli.scope()))
    }

    /// The store [`Self::held_view`] read, while the hold is on and a
    /// `state.db` exists.
    fn held_store(&self) -> Option<&StateStore> {
        if !self.store_held.get() {
            return None;
        }
        self.held_view().records.0.as_ref()
    }

    /// The refusal for a held run whose existing `state.db` could not be read
    /// without writing to it.
    fn held_readable(&self) -> anyhow::Result<()> {
        if self.store_held.get() {
            self.held_view().readable()?;
        }
        Ok(())
    }

    /// A package context over this run's installed-state memo and `state`,
    /// for a run that reads the store without opening it through
    /// [`Self::state`], which creates one that is not there.
    pub(in crate::cli) fn package_context_over<'s>(
        &'s self,
        state: &'s dyn cfgd_core::providers::PackageStateStore,
    ) -> cfgd_core::providers::PackageContext<'s> {
        cfgd_core::providers::PackageContext::with_shared_enumerations(
            self.printer,
            state,
            &self.enumerations,
        )
    }

    /// A package context over this run's ONE installed-state memo.
    ///
    /// Every context built here lends [`Self::enumerations`], so module
    /// resolution (which asks which manager already holds a bare entry), the
    /// profile planner and the reconciler's elision all read one enumeration
    /// per manager for the whole invocation — a context built with
    /// `PackageContext::new` owns a memo of its own and re-asks.
    pub(in crate::cli) fn package_context(
        &self,
    ) -> anyhow::Result<cfgd_core::providers::PackageContext<'_>> {
        if self.store_held.get() {
            return Ok(self.package_context_over(&self.held_view().records));
        }
        let state = self.state()?;
        Ok(
            cfgd_core::providers::PackageContext::with_shared_enumerations(
                self.printer,
                state,
                &self.enumerations,
            ),
        )
    }

    /// The config-free provider registry, built at most once.
    ///
    /// This is the registry a module-only path asks for: it carries the
    /// built-in managers and every configurator this host supports, and knows
    /// nothing of the profile's custom managers or secret backend. A path that
    /// needs the config-aware registry takes the one
    /// [`super::helpers::resolve_desired_state`] hands back instead.
    pub(in crate::cli) fn base_registry(&self) -> &ProviderRegistry {
        self.base_registry.get_or_init(build_registry)
    }

    /// Merge every manifest file `spec` references into its inline package
    /// lists, reading each file at most once per run.
    ///
    /// `sources` is the merged profile's own claims, extended in place with
    /// one claim per folded package so a row recorded for a package that only
    /// a source-declared Brewfile names still points at that source.
    pub(in crate::cli) fn resolve_manifest_packages(
        &self,
        spec: &mut PackagesSpec,
        sources: &mut LayerSources,
    ) -> cfgd_core::errors::Result<()> {
        packages::resolve_manifest_packages_cached(spec, sources, &self.config_dir, &self.manifests)
    }
}

#[cfg(any(test, feature = "test-helpers"))]
impl RunContext<'_> {
    /// Run `verb` against a context over `cli`'s config as it stands now, read
    /// the way the process reads it before dispatch.
    pub fn for_test<T>(cli: &Cli, printer: &Printer, verb: impl FnOnce(&RunContext<'_>) -> T) -> T {
        let startup = StartupDocument::load(&cli.config);
        verb(&RunContext::new(cli, printer, &startup))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cfgd_core::test_helpers::test_printer;

    const CONFIG_YAML: &str = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n";
    const PROFILE_YAML: &str = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  env:\n    - name: editor\n      value: vim\n";

    fn cli_in(dir: &Path) -> Cli {
        Cli {
            config: dir.join("cfgd.yaml"),
            config_explicit: false,
            profile: None,
            verbose: 0,
            quiet: true,
            no_color: true,
            color: crate::cli::ColorWhen::Auto,
            output: crate::cli::OutputFormatArg(cfgd_core::output::OutputFormat::Table),
            list_envelope: false,
            hints: false,
            no_hints: false,
            theme: None,
            mask_env_values: None,
            migration_policy: None,
            update_policy: None,
            jsonpath: None,
            yes: false,
            state_dir: None,
            config_dir: None,
            cache_dir: None,
            runtime_dir: None,
            scope_arg: crate::cli::ScopeArg::User,
            command: None,
        }
    }

    fn write_config(dir: &Path) {
        std::fs::write(dir.join("cfgd.yaml"), CONFIG_YAML).unwrap();
        std::fs::create_dir_all(dir.join("profiles")).unwrap();
        std::fs::write(dir.join("profiles").join("default.yaml"), PROFILE_YAML).unwrap();
    }

    #[test]
    fn the_run_never_rereads_the_config_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path());
        let printer = test_printer();
        let cli = cli_in(dir.path());
        let startup = StartupDocument::load(&cli.config);
        let ctx = RunContext::new(&cli, &printer, &startup);

        let first = ctx.config().unwrap() as *const CfgdConfig;
        // The file is gone: a `config()` that still answers read nothing from
        // disk.
        std::fs::remove_file(dir.path().join("cfgd.yaml")).unwrap();
        let second = ctx.config().unwrap() as *const CfgdConfig;

        assert_eq!(first, second);
    }

    #[test]
    fn the_profile_is_resolved_once_per_run() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path());
        let printer = test_printer();
        let cli = cli_in(dir.path());
        let startup = StartupDocument::load(&cli.config);
        let ctx = RunContext::new(&cli, &printer, &startup);

        let (_, name, resolved) = ctx.config_and_profile().unwrap();
        assert_eq!(name, "default");
        let first = resolved as *const ResolvedProfile;

        std::fs::remove_dir_all(dir.path().join("profiles")).unwrap();
        let (_, _, resolved) = ctx.config_and_profile().unwrap();

        assert_eq!(first, resolved as *const ResolvedProfile);
    }

    #[test]
    fn the_state_store_is_opened_once_per_run() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path());
        let state_dir = dir.path().join("state");
        let printer = test_printer();
        let mut cli = cli_in(dir.path());
        cli.state_dir = Some(state_dir.clone());
        let startup = StartupDocument::load(&cli.config);
        let ctx = RunContext::new(&cli, &printer, &startup);

        let first = ctx.state().unwrap() as *const StateStore;
        // A second open would re-create the directory it was told to use, so
        // the directory still being absent afterwards is the proof there was
        // no second open. Unix-only: Windows refuses to unlink the database
        // file the held connection keeps open, so there the proof rests on
        // the OnceCell identity alone.
        #[cfg(unix)]
        std::fs::remove_dir_all(&state_dir).unwrap();
        let second = ctx.state().unwrap() as *const StateStore;

        assert_eq!(first, second);
        #[cfg(unix)]
        assert!(!state_dir.exists());
    }

    #[test]
    fn the_base_registry_is_built_once_per_run() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path());
        let printer = test_printer();
        let cli = cli_in(dir.path());
        let startup = StartupDocument::load(&cli.config);
        let ctx = RunContext::new(&cli, &printer, &startup);

        let first = ctx.base_registry() as *const ProviderRegistry;
        let second = ctx.base_registry() as *const ProviderRegistry;

        assert_eq!(first, second);
    }

    /// The parse is memoized, but the deprecation drain is a separate step, so
    /// reusing the parse cannot start printing notices where the old
    /// name-only path printed none — nor print them twice where it printed
    /// once.
    #[test]
    fn deprecations_are_announced_once_and_never_by_the_name_only_read() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path());
        std::fs::write(
            dir.path().join("cfgd.yaml"),
            format!("{CONFIG_YAML}  theme:\n    overrides:\n      subheader: red\n"),
        )
        .unwrap();
        let (printer, buf) = cfgd_core::output::Printer::for_test();
        let cli = cli_in(dir.path());
        let startup = StartupDocument::load(&cli.config);
        let ctx = RunContext::new(&cli, &printer, &startup);

        assert_eq!(ctx.active_profile_name(), "default");
        assert!(
            cfgd_core::test_helpers::captured_text(&buf).is_empty(),
            "the name-only read parsed the config but must not announce it"
        );

        ctx.config().unwrap();
        ctx.config().unwrap();
        let out = cfgd_core::test_helpers::captured_text(&buf);
        assert_eq!(out.matches("theme.overrides.subheader").count(), 1, "{out}");
    }

    /// A run's config is the startup document's own parse, by reference.
    #[test]
    fn the_run_reads_the_startup_document() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path());
        let printer = test_printer();
        let cli = cli_in(dir.path());
        let startup = StartupDocument::load(&cli.config);
        let ctx = RunContext::new(&cli, &printer, &startup);

        assert!(std::ptr::eq(
            ctx.config().unwrap(),
            startup.config().unwrap()
        ));
    }
}
