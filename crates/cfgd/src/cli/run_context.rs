use std::cell::{Cell, OnceCell};
use std::path::{Path, PathBuf};

use cfgd_core::config::{CfgdConfig, PackagesSpec, ResolvedProfile};
use cfgd_core::output::Printer;
use cfgd_core::providers::ProviderRegistry;
use cfgd_core::state::StateStore;

use super::helpers::{config_dir, resolve_profile_for};
use super::registry::{build_registry, open_state_store};
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
/// Scoped to ONE run: each `cmd_*` builds a context at its top and drops it when
/// it returns. Construction is pure (it copies two references and derives the
/// config directory), so a daemon tick can hold one per tick without paying for
/// slots that tick does not use, and nothing here can outlive the config it
/// describes.
///
/// Not `Sync` by construction — the cells are single-threaded. Concurrent phases
/// receive the resolved objects (`&StateStore`, `&ProviderRegistry`), never the
/// context.
pub(in crate::cli) struct RunContext<'a> {
    cli: &'a Cli,
    printer: &'a Printer,
    config_dir: PathBuf,
    /// `cli.config` as parsed, with its deprecation notices still intact.
    config: OnceCell<CfgdConfig>,
    /// Whether those notices have already been surfaced. The drain is once per
    /// run and belongs to the first caller that reads the config for real —
    /// a command that only wants the active profile NAME must not print them,
    /// which is why the parse and the drain are separate steps.
    deprecations_drained: Cell<bool>,
    profile: OnceCell<(String, ResolvedProfile)>,
    state: OnceCell<StateStore>,
    base_registry: OnceCell<ProviderRegistry>,
    manifests: ManifestCache,
}

impl<'a> RunContext<'a> {
    pub(in crate::cli) fn new(cli: &'a Cli, printer: &'a Printer) -> Self {
        Self {
            cli,
            printer,
            config_dir: config_dir(cli),
            config: OnceCell::new(),
            deprecations_drained: Cell::new(false),
            profile: OnceCell::new(),
            state: OnceCell::new(),
            base_registry: OnceCell::new(),
            manifests: ManifestCache::default(),
        }
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

    /// The run's config, parsed at most once, WITHOUT surfacing its deprecation
    /// notices. Only the callers that need nothing but a name off the config
    /// (the active profile a module-only run stamps into `CFGD_PROFILE`) read
    /// through here.
    fn config_unannounced(&self) -> cfgd_core::errors::Result<&CfgdConfig> {
        if let Some(cfg) = self.config.get() {
            return Ok(cfg);
        }
        let cfg = cfgd_core::config::load_config(&self.cli.config)?;
        Ok(self.config.get_or_init(|| cfg))
    }

    /// The run's config, parsed at most once, with its deprecation notices
    /// surfaced exactly once.
    pub(in crate::cli) fn config(&self) -> cfgd_core::errors::Result<&CfgdConfig> {
        let cfg = self.config_unannounced()?;
        if !self.deprecations_drained.replace(true) {
            for msg in &cfg.deprecations {
                self.printer.deprecation(msg);
            }
        }
        Ok(cfg)
    }

    /// The run's config, the name of the profile in force, and that profile's
    /// resolution — the reference-returning form of
    /// [`super::helpers::load_config_and_profile`], resolved at most once.
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

    /// Best-effort name of the profile a module-only command runs under: the
    /// explicit `--profile`, else the config's active profile, else
    /// `"unknown"`.
    ///
    /// Module-only commands never resolve a profile, but the scripts they run
    /// (a `patch.script` filter, a lifecycle hook) still receive
    /// `CFGD_PROFILE`, so the name must be the real one wherever the config
    /// knows it. Reads the run's already-parsed config when there is one, and
    /// otherwise parses it into the same slot rather than off to the side.
    pub(in crate::cli) fn active_profile_name(&self) -> String {
        if let Some(p) = self.cli.profile.as_deref() {
            return p.to_string();
        }
        self.config_unannounced()
            .ok()
            .and_then(|cfg| cfg.active_profile().ok())
            .map(str::to_string)
            .unwrap_or_else(|| "unknown".to_string())
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
    pub(in crate::cli) fn resolve_manifest_packages(
        &self,
        spec: &mut PackagesSpec,
    ) -> cfgd_core::errors::Result<()> {
        packages::resolve_manifest_packages_cached(spec, &self.config_dir, &self.manifests)
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
            jsonpath: None,
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
    fn the_config_is_parsed_once_per_run() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path());
        let printer = test_printer();
        let cli = cli_in(dir.path());
        let ctx = RunContext::new(&cli, &printer);

        let first = ctx.config().unwrap() as *const CfgdConfig;
        // The file is gone: a second parse could not succeed, so a second
        // `config()` answering at all is the memo answering.
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
        let ctx = RunContext::new(&cli, &printer);

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
        let ctx = RunContext::new(&cli, &printer);

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
        let ctx = RunContext::new(&cli, &printer);

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
        let ctx = RunContext::new(&cli, &printer);

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
}
