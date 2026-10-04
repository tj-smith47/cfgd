use std::cell::{Cell, OnceCell};
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
    pub(in crate::cli) fn state(&self) -> anyhow::Result<&StateStore> {
        if let Some(state) = self.state.get() {
            return Ok(state);
        }
        let state = open_state_store(self.cli.state_dir.as_deref(), self.cli.scope())?;
        Ok(self.state.get_or_init(|| state))
    }

    /// The run's state store, or `None` when it cannot be opened — for the
    /// advisory paths that record if they can and carry on if they cannot.
    pub(in crate::cli) fn state_opt(&self) -> Option<&StateStore> {
        self.state().ok()
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
