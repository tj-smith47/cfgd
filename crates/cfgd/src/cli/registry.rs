use super::*;

use cfgd_core::PathDisplayExt;

// --- Provider registry, daemon hooks, state store ---

/// Extract secret backend name and age key path from config.
/// Returns ("sops", None) as defaults when no secrets config is present.
pub(in crate::cli) fn secret_backend_from_config(
    cfg: Option<&CfgdConfig>,
) -> (String, Option<PathBuf>) {
    if let Some(cfg) = cfg
        && let Some(ref secrets_cfg) = cfg.spec.secrets
    {
        let name = secrets_cfg.backend.as_str().to_string();
        let key = secrets_cfg.sops.as_ref().and_then(|s| s.age_key.clone());
        (name, key)
    } else {
        ("sops".to_string(), None)
    }
}

pub(in crate::cli) fn build_registry() -> ProviderRegistry {
    build_registry_with_config(None)
}

/// DaemonHooks implementation for the workstation binary.
/// Provides concrete provider wiring so cfgd-core's daemon can plan packages/files.
pub(in crate::cli) struct WorkstationDaemonHooks;

impl cfgd_core::daemon::DaemonHooks for WorkstationDaemonHooks {
    fn build_registry(&self, config: &CfgdConfig) -> ProviderRegistry {
        build_registry_with_config(Some(config))
    }

    fn plan_files(
        &self,
        config_dir: &std::path::Path,
        resolved: &ResolvedProfile,
    ) -> cfgd_core::errors::Result<Vec<FileAction>> {
        let fm = build_compliance_file_manager(config_dir, resolved)?;
        fm.plan(&resolved.merged)
    }

    fn plan_packages(
        &self,
        profile: &cfgd_core::config::MergedProfile,
        managers: &[&dyn cfgd_core::providers::PackageManager],
        cfgd_installed: &std::collections::HashSet<String>,
    ) -> cfgd_core::errors::Result<Vec<cfgd_core::providers::PackageAction>> {
        // The daemon reconcile is a full, unscoped run, so forward the real
        // tracked set: it prunes packages cfgd installed that have left the
        // desired set (the safety invariant bounds it to cfgd-owned packages).
        // Profile-scoped (`&[]`): the daemon adds module packages separately via
        // `reconciler.plan` as `Action::Module`, so this planner stays profile-only.
        packages::plan_packages(profile, &[], managers, cfgd_installed)
    }

    fn extend_registry_custom_managers(
        &self,
        registry: &mut ProviderRegistry,
        packages: &cfgd_core::config::PackagesSpec,
    ) {
        registry
            .package_managers
            .extend(crate::packages::custom_managers(&packages.custom));
    }

    fn build_file_manager(
        &self,
        config_dir: &std::path::Path,
        resolved: &ResolvedProfile,
    ) -> cfgd_core::errors::Result<Option<Box<dyn cfgd_core::providers::FileManager>>> {
        Ok(Some(Box::new(build_compliance_file_manager(
            config_dir, resolved,
        )?)))
    }

    fn expand_tilde(&self, path: &std::path::Path) -> std::path::PathBuf {
        cfgd_core::expand_tilde(path)
    }

    fn prune_orphaned_packages(
        &self,
        orphans: &[cfgd_core::providers::OrphanedPackage],
        printer: &cfgd_core::output::Printer,
    ) -> Vec<(String, String)> {
        crate::packages::prune_orphaned_packages(orphans, printer)
    }
}

pub(in crate::cli) fn build_registry_with_profile(
    spec: &cfgd_core::config::PackagesSpec,
) -> ProviderRegistry {
    build_registry_with_config_and_packages(None, Some(spec))
}

/// Build a `CfgdFileManager` configured for read-only content comparison.
///
/// Wires the global file strategy and secret backend/providers so templates that
/// reference `${secret:...}` render to the same bytes the apply path would write,
/// making compliance content checks compare against the true desired content.
/// Shared by the compliance/checkin CLI callers and the daemon's compliance hook
/// so every surface content-checks identically.
pub(in crate::cli) fn build_compliance_file_manager(
    config_dir: &std::path::Path,
    resolved: &ResolvedProfile,
) -> cfgd_core::errors::Result<CfgdFileManager> {
    let mut fm = CfgdFileManager::new(config_dir, resolved)?;
    let cfg = config::load_config(&config_dir.join("cfgd.yaml"))?;
    fm.set_global_strategy(cfg.spec.file_strategy);
    let (backend_name, age_key_path) = secret_backend_from_config(Some(&cfg));
    let backend = secrets::build_secret_backend(&backend_name, age_key_path, Some(config_dir));
    let providers = secrets::build_secret_providers();
    fm.set_secret_providers(Some(backend), providers);
    Ok(fm)
}

pub(in crate::cli) fn build_registry_with_config(cfg: Option<&CfgdConfig>) -> ProviderRegistry {
    build_registry_with_config_and_packages(cfg, None)
}

pub(in crate::cli) fn build_registry_with_config_and_packages(
    cfg: Option<&CfgdConfig>,
    packages: Option<&cfgd_core::config::PackagesSpec>,
) -> ProviderRegistry {
    let mut registry = ProviderRegistry::new();
    registry.package_managers = packages::all_package_managers();

    // Register system configurators based on OS
    use crate::system::*;

    // ShellConfigurator: `chsh` on Unix, Windows Terminal settings.json on Windows
    if cfg!(unix) || cfg!(windows) {
        registry
            .system_configurators
            .push(Box::new(ShellConfigurator));
    }

    if cfg!(target_os = "macos") {
        registry
            .system_configurators
            .push(Box::new(MacosDefaultsConfigurator));
        registry
            .system_configurators
            .push(Box::new(LaunchAgentConfigurator));
    }

    if cfg!(target_os = "linux") {
        registry
            .system_configurators
            .push(Box::new(SystemdUnitConfigurator::default()));
        // Linux desktop configurators — each checks CLI availability at runtime via is_available()
        registry
            .system_configurators
            .push(Box::new(GsettingsConfigurator));
        registry
            .system_configurators
            .push(Box::new(KdeConfigConfigurator));
        registry
            .system_configurators
            .push(Box::new(XfconfConfigurator));
    }

    // Environment configurator is available on Unix and Windows
    if cfg!(unix) || cfg!(windows) {
        registry
            .system_configurators
            .push(Box::new(EnvironmentConfigurator));
    }

    // Windows registry configurator
    if cfg!(windows) {
        registry
            .system_configurators
            .push(Box::new(WindowsRegistryConfigurator));
    }

    // Windows service configurator
    if cfg!(windows) {
        registry
            .system_configurators
            .push(Box::new(WindowsServiceConfigurator));
    }

    // SSH key configurator — available unconditionally (ssh-keygen on all platforms)
    registry
        .system_configurators
        .push(Box::new(SshKeysConfigurator));

    // GPG key configurator — available on any platform where gpg is installed
    if cfgd_core::command_available("gpg") {
        registry
            .system_configurators
            .push(Box::new(GpgKeysConfigurator));
    }

    // Git configurator — cross-platform, gated on git being available at runtime
    if cfgd_core::command_available("git") {
        registry
            .system_configurators
            .push(Box::new(GitConfigurator));
    }

    // Node/infrastructure system configurators (Linux-only, gated at compile time)
    #[cfg(unix)]
    {
        registry
            .system_configurators
            .push(Box::new(SysctlConfigurator));
        registry
            .system_configurators
            .push(Box::new(KernelModuleConfigurator));
        registry
            .system_configurators
            .push(Box::new(ContainerdConfigurator));
        registry
            .system_configurators
            .push(Box::new(KubeletConfigurator));
        registry
            .system_configurators
            .push(Box::new(AppArmorConfigurator));
        registry
            .system_configurators
            .push(Box::new(SeccompConfigurator));
        registry
            .system_configurators
            .push(Box::new(CertificateConfigurator));
    }

    // Register secret backend and providers
    let (backend_name, age_key_path) = secret_backend_from_config(cfg);
    registry.secret_backend = Some(secrets::build_secret_backend(
        &backend_name,
        age_key_path,
        None,
    ));
    registry.secret_providers = secrets::build_secret_providers();

    // Set global file strategy from config
    if let Some(cfg) = cfg {
        registry.default_file_strategy = cfg.spec.file_strategy;
    }

    // Extend with custom package managers from profile packages spec
    if let Some(spec) = packages {
        registry
            .package_managers
            .extend(packages::custom_managers(&spec.custom));
    }

    registry
}

/// Build the cfgd-installed package set (`"<manager>/<package>"` entries) from
/// tracked state, for [`packages::plan_packages`] to bound declarative prune.
pub(in crate::cli) fn cfgd_installed_packages(
    state: &StateStore,
) -> anyhow::Result<std::collections::HashSet<String>> {
    Ok(state
        .managed_package_ids()?
        .into_iter()
        .map(|(mgr, pkg)| format!("{mgr}/{pkg}"))
        .collect())
}

pub(in crate::cli) fn open_state_store(state_dir: Option<&Path>) -> anyhow::Result<StateStore> {
    if let Some(dir) = state_dir {
        Ok(StateStore::open_in_dir(dir)?)
    } else {
        Ok(StateStore::open_default()?)
    }
}

// --- Secret backend resolution ---

/// Resolve the secret backend from config, check availability, and validate the file exists.
/// Returns a registry whose `secret_backend` is guaranteed `Some`.
pub(in crate::cli) fn resolve_secret_backend(
    cli: &Cli,
    file: &Path,
) -> anyhow::Result<ProviderRegistry> {
    let cfg = if cli.config.exists() {
        Some(config::load_config(&cli.config)?)
    } else {
        None
    };

    let mut registry = build_registry_with_config(cfg.as_ref());

    // Rebuild secret backend with config dir so sops can find .sops.yaml
    let cd = config_dir(cli);
    let (backend_name, age_key_path) = secret_backend_from_config(cfg.as_ref());
    registry.secret_backend = Some(secrets::build_secret_backend(
        &backend_name,
        age_key_path,
        Some(&cd),
    ));

    if !file.exists() {
        anyhow::bail!("File not found: {}", file.posix());
    }

    match registry.secret_backend {
        Some(ref backend) if !backend.is_available() => {
            anyhow::bail!("{}: not installed", backend.name());
        }
        None => anyhow::bail!("No secret backend configured"),
        _ => {}
    }

    Ok(registry)
}

/// Shorthand: resolve secret backend and extract it in one call.
pub(in crate::cli) fn get_secret_backend(
    cli: &Cli,
    file: &Path,
) -> anyhow::Result<Box<dyn SecretBackend>> {
    let registry = resolve_secret_backend(cli, file)?;
    registry
        .secret_backend
        .ok_or_else(|| anyhow::anyhow!("No secret backend configured"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cfgd_core::output::OutputFormat;

    fn parse_config(yaml: &str) -> CfgdConfig {
        config::parse_config(yaml, Path::new("cfgd.yaml")).expect("fixture config must parse")
    }

    fn cli_for(config: PathBuf) -> Cli {
        Cli {
            config,
            profile: None,
            verbose: 0,
            quiet: true,
            no_color: true,
            output: crate::cli::OutputFormatArg(OutputFormat::Table),
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

    // --- secret_backend_from_config ---

    #[test]
    fn secret_backend_defaults_to_sops_with_no_key_when_config_absent() {
        let (name, key) = secret_backend_from_config(None);
        assert_eq!(name, "sops", "absent config must default to sops backend");
        assert_eq!(key, None, "absent config must yield no age key path");
    }

    #[test]
    fn secret_backend_defaults_to_sops_when_secrets_section_omitted() {
        let cfg = parse_config(
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n",
        );
        let (name, key) = secret_backend_from_config(Some(&cfg));
        assert_eq!(name, "sops");
        assert_eq!(key, None);
    }

    #[test]
    fn secret_backend_extracts_named_backend_and_age_key() {
        let cfg = parse_config(
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n  secrets:\n    backend: age\n    sops:\n      ageKey: /keys/age.txt\n",
        );
        let (name, key) = secret_backend_from_config(Some(&cfg));
        assert_eq!(name, "age", "must surface the configured backend verbatim");
        assert_eq!(
            key,
            Some(PathBuf::from("/keys/age.txt")),
            "must surface the configured sops.ageKey"
        );
    }

    #[test]
    fn secret_backend_named_backend_without_sops_block_has_no_key() {
        let cfg = parse_config(
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n  secrets:\n    backend: vault\n",
        );
        let (name, key) = secret_backend_from_config(Some(&cfg));
        assert_eq!(name, "vault");
        assert_eq!(key, None, "no sops block means no age key path");
    }

    // --- build_registry_with_config ---

    #[test]
    fn build_registry_registers_package_managers_and_secret_backend() {
        let registry = build_registry_with_config(None);
        assert!(
            !registry.package_managers.is_empty(),
            "registry must register at least one package manager"
        );
        let backend = registry
            .secret_backend
            .as_ref()
            .expect("registry must always carry a secret backend");
        assert_eq!(
            backend.name(),
            "sops",
            "default registry backend must be sops"
        );
        assert!(
            !registry.secret_providers.is_empty(),
            "registry must register secret providers"
        );
    }

    #[test]
    fn build_registry_uses_age_backend_when_config_selects_age() {
        let cfg = parse_config(
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n  secrets:\n    backend: age\n",
        );
        let registry = build_registry_with_config(Some(&cfg));
        let backend = registry
            .secret_backend
            .as_ref()
            .expect("registry must carry a secret backend");
        assert_eq!(
            backend.name(),
            "age",
            "config-selected age backend must win"
        );
    }

    #[test]
    fn build_registry_no_args_delegates_to_config_none() {
        // build_registry() is the zero-arg convenience over build_registry_with_config(None).
        let a = build_registry();
        let b = build_registry_with_config(None);
        assert_eq!(
            a.package_managers.len(),
            b.package_managers.len(),
            "zero-arg build_registry must match build_registry_with_config(None)"
        );
        assert_eq!(
            a.secret_providers.len(),
            b.secret_providers.len(),
            "secret provider count must match"
        );
    }

    // --- cfgd_installed_packages ---

    #[test]
    fn cfgd_installed_packages_formats_manager_slash_package() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = StateStore::open_in_dir(dir.path()).expect("open state");
        state
            .upsert_package_resource("brew/ripgrep", "local", None, None)
            .expect("track package");

        let set = cfgd_installed_packages(&state).expect("collect installed");
        assert!(
            set.contains("brew/ripgrep"),
            "installed set must contain the manager/package id, got: {set:?}"
        );
    }

    #[test]
    fn cfgd_installed_packages_empty_when_no_tracked_packages() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = StateStore::open_in_dir(dir.path()).expect("open state");
        let set = cfgd_installed_packages(&state).expect("collect installed");
        assert!(
            set.is_empty(),
            "no tracked packages must yield an empty set, got: {set:?}"
        );
    }

    // --- resolve_secret_backend / get_secret_backend ---

    #[test]
    fn resolve_secret_backend_errors_when_file_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cli = cli_for(dir.path().join("cfgd.yaml"));
        let missing = dir.path().join("absent.enc");

        // ProviderRegistry is not Debug, so match rather than expect_err.
        let err = match resolve_secret_backend(&cli, &missing) {
            Ok(_) => panic!("missing target file must error"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("File not found"),
            "error must name the missing file, got: {err}"
        );
    }

    #[test]
    fn get_secret_backend_errors_when_file_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cli = cli_for(dir.path().join("cfgd.yaml"));
        let missing = dir.path().join("absent.enc");

        // Box<dyn SecretBackend> is not Debug, so match rather than expect_err.
        let err = match get_secret_backend(&cli, &missing) {
            Ok(_) => panic!("missing target file must error"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("File not found"),
            "error must name the missing file, got: {err}"
        );
    }

    #[test]
    fn open_state_store_honors_explicit_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = open_state_store(Some(dir.path())).expect("open in explicit dir");
        // Round-trip a write to prove the store at this dir is live and usable.
        state
            .upsert_package_resource("apt/curl", "local", None, None)
            .expect("write to explicit-dir store");
        let set = cfgd_installed_packages(&state).expect("read back");
        assert!(set.contains("apt/curl"));
    }
}
