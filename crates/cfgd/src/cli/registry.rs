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
        let mut fm = CfgdFileManager::new(config_dir, resolved)?;
        let cfg = config::load_config(&config_dir.join("cfgd.yaml"))?;
        fm.set_global_strategy(cfg.spec.file_strategy);
        let (backend_name, age_key_path) = secret_backend_from_config(Some(&cfg));
        let backend = secrets::build_secret_backend(&backend_name, age_key_path, Some(config_dir));
        let providers = secrets::build_secret_providers();
        fm.set_secret_providers(Some(backend), providers);
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
        packages::plan_packages(profile, managers, cfgd_installed)
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
