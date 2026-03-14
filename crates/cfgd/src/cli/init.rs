use std::path::{Path, PathBuf};

use super::*;

// --- Bootstrap State (for resumable init) ---

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
struct BootstrapState {
    repo_url: Option<String>,
    config_dir: String,
    profile: Option<String>,
    phase: BootstrapPhase,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
enum BootstrapPhase {
    Clone,
    ProfileSelect,
    SecretsSetup,
    Plan,
    Apply,
    Verify,
    DaemonInstall,
    Complete,
}

impl BootstrapPhase {
    fn display_name(&self) -> &'static str {
        match self {
            Self::Clone => "Clone repository",
            Self::ProfileSelect => "Select profile",
            Self::SecretsSetup => "Secrets setup",
            Self::Plan => "Generate plan",
            Self::Apply => "Apply configuration",
            Self::Verify => "Verify resources",
            Self::DaemonInstall => "Daemon setup",
            Self::Complete => "Complete",
        }
    }
}

fn save_bootstrap_state(config_dir: &Path, state: &BootstrapState) -> anyhow::Result<()> {
    let state_path = config_dir.join(BOOTSTRAP_STATE_FILE);
    let json = serde_json::to_string_pretty(state)?;
    std::fs::write(&state_path, json)?;
    Ok(())
}

fn load_bootstrap_state(config_dir: &Path) -> Option<BootstrapState> {
    let state_path = config_dir.join(BOOTSTRAP_STATE_FILE);
    if !state_path.exists() {
        return None;
    }
    let contents = std::fs::read_to_string(&state_path).ok()?;
    serde_json::from_str(&contents).ok()
}

fn clear_bootstrap_state(config_dir: &Path) {
    let state_path = config_dir.join(BOOTSTRAP_STATE_FILE);
    let _ = std::fs::remove_file(&state_path);
}

// --- Init Command ---

pub(super) fn cmd_init(
    printer: &Printer,
    from: Option<&str>,
    branch: &str,
    theme: Option<&str>,
) -> anyhow::Result<()> {
    printer.header("Initialize cfgd");

    // Check prerequisites
    if !check_prerequisites(printer) {
        return Ok(());
    }

    let config_dir = if let Some(url) = from {
        // Clone from remote — check for cfgd-source.yaml
        let cloned_dir = init_from_remote(printer, url, branch)?;
        let cloned_dir = match cloned_dir {
            Some(dir) => dir,
            None => return Ok(()),
        };

        // Source detection: if the cloned repo has cfgd-source.yaml, enter source-aware flow
        match cfgd_core::sources::detect_source_manifest(&cloned_dir) {
            Ok(Some(manifest)) => {
                return init_from_source(printer, url, &cloned_dir, manifest);
            }
            Ok(None) => {
                // Plain config repo — continue with normal flow
            }
            Err(e) => {
                printer.warning(&format!(
                    "Found cfgd-source.yaml but could not parse it: {}",
                    e
                ));
                printer.info("Continuing as a plain config repo");
            }
        }

        Some(cloned_dir)
    } else {
        // Interactive local init wizard
        init_local(printer)?
    };

    let config_dir = match config_dir {
        Some(dir) => dir,
        None => return Ok(()),
    };

    // Check for resumable bootstrap
    let mut state = load_bootstrap_state(&config_dir).unwrap_or(BootstrapState {
        repo_url: from.map(|s| s.to_string()),
        config_dir: config_dir.display().to_string(),
        profile: None,
        phase: BootstrapPhase::ProfileSelect,
    });

    if state.phase != BootstrapPhase::ProfileSelect {
        printer.info(&format!(
            "Resuming bootstrap from: {}",
            state.phase.display_name()
        ));
    }

    // Phase: Profile selection
    if state.phase == BootstrapPhase::ProfileSelect {
        let profile = bootstrap_profile_select(&config_dir, printer)?;
        match profile {
            Some(p) => {
                state.profile = Some(p);
                state.phase = BootstrapPhase::SecretsSetup;
                save_bootstrap_state(&config_dir, &state)?;
            }
            None => return Ok(()),
        }
    }

    let profile_name = match state.profile {
        Some(ref p) => p.clone(),
        None => {
            anyhow::bail!("No profile selected");
        }
    };

    // Ensure cfgd.yaml exists with the selected profile
    let config_path = config_dir.join("cfgd.yaml");
    ensure_config_file(
        &config_dir,
        &config_path,
        &profile_name,
        from,
        branch,
        theme,
    )?;

    // Phase: Secrets setup
    if state.phase == BootstrapPhase::SecretsSetup {
        bootstrap_secrets_setup(&config_dir, printer)?;
        state.phase = BootstrapPhase::Plan;
        save_bootstrap_state(&config_dir, &state)?;
    }

    // Pre-bootstrap diagnostics
    if state.phase == BootstrapPhase::Plan {
        printer.newline();
        run_pre_bootstrap_diagnostics(printer)?;
    }

    // Phase: Plan
    if state.phase == BootstrapPhase::Plan {
        printer.newline();
        printer.header("Bootstrap Plan");

        let cfg = config::load_config(&config_path)?;
        let profiles_dir = config_dir.join("profiles");
        let resolved = config::resolve_profile(&profile_name, &profiles_dir)?;
        let registry = build_registry_with_config(Some(&cfg));
        let store = open_state_store()?;
        let reconciler = Reconciler::new(&registry, &store);

        let all_managers: Vec<&dyn cfgd_core::providers::PackageManager> = registry
            .package_managers
            .iter()
            .map(|m| m.as_ref())
            .collect();
        let pkg_actions = packages::plan_packages(&resolved.merged, &all_managers)?;

        let fm = CfgdFileManager::new(&config_dir, &resolved)?;
        let file_actions = fm.plan(&resolved.merged)?;

        let plan = reconciler.plan(&resolved, file_actions, pkg_actions, Vec::new())?;

        for phase in &plan.phases {
            let items = reconciler::format_plan_items(phase);
            printer.plan_phase(phase.name.display_name(), &items);
        }

        let total = plan.total_actions();
        printer.newline();
        if total == 0 {
            printer.success("Nothing to do — system is already configured");
            state.phase = BootstrapPhase::Verify;
            save_bootstrap_state(&config_dir, &state)?;
        } else {
            printer.info(&format!("{} action(s) planned", total));
            printer.newline();

            let confirmed = printer
                .prompt_confirm("Apply these changes?")
                .unwrap_or(false);
            if !confirmed {
                printer.info("Aborted — run 'cfgd init' again to resume");
                return Ok(());
            }

            state.phase = BootstrapPhase::Apply;
            save_bootstrap_state(&config_dir, &state)?;
        }
    }

    // Phase: Apply
    if state.phase == BootstrapPhase::Apply {
        printer.newline();
        printer.header("Applying Configuration");

        let cfg = config::load_config(&config_path)?;
        let profiles_dir = config_dir.join("profiles");
        let resolved = config::resolve_profile(&profile_name, &profiles_dir)?;
        let mut registry = build_registry_with_config(Some(&cfg));
        let store = open_state_store()?;

        let all_managers: Vec<&dyn cfgd_core::providers::PackageManager> = registry
            .package_managers
            .iter()
            .map(|m| m.as_ref())
            .collect();
        let pkg_actions = packages::plan_packages(&resolved.merged, &all_managers)?;

        let mut fm = CfgdFileManager::new(&config_dir, &resolved)?;
        // Set up secret providers for template rendering during apply
        let (backend_name, age_key_path) = secret_backend_from_config(Some(&cfg));
        fm.set_secret_providers(
            Some(secrets::build_secret_backend(&backend_name, age_key_path)),
            secrets::build_secret_providers(),
        );
        let file_actions = fm.plan(&resolved.merged)?;

        // Register the file manager so the reconciler delegates through the trait
        registry.file_manager = Some(Box::new(fm));

        let reconciler = Reconciler::new(&registry, &store);
        let plan = reconciler.plan(&resolved, file_actions, pkg_actions, Vec::new())?;

        let result = reconciler.apply(&plan, &resolved, &config_dir, printer, None, &[])?;

        printer.newline();
        let status = print_apply_result(&result, printer);
        if status == cfgd_core::state::ApplyStatus::Partial {
            printer.info("Failed actions can be retried with 'cfgd apply'");
        } else if status == cfgd_core::state::ApplyStatus::Failed {
            printer.info("Review errors above and run 'cfgd init' to retry");
            return Ok(());
        }

        state.phase = BootstrapPhase::Verify;
        save_bootstrap_state(&config_dir, &state)?;
    }

    // Phase: Verify
    if state.phase == BootstrapPhase::Verify {
        printer.newline();
        printer.header("Verification");

        let profiles_dir = config_dir.join("profiles");
        let resolved = config::resolve_profile(&profile_name, &profiles_dir)?;
        let registry = build_registry_with_profile(&resolved.merged.packages);
        let store = open_state_store()?;

        let results = reconciler::verify(&resolved, &registry, &store, printer, &[])?;

        if !results.is_empty() {
            let (pass_count, fail_count) = print_verify_results(&results, printer);
            printer.newline();
            if fail_count == 0 {
                printer.success(&format!("All {} resource(s) verified", pass_count));
            } else {
                printer.warning(&format!(
                    "{} passed, {} failed — run 'cfgd apply' to fix",
                    pass_count, fail_count
                ));
            }
        }

        state.phase = BootstrapPhase::DaemonInstall;
        save_bootstrap_state(&config_dir, &state)?;
    }

    // Phase: Daemon install (optional)
    if state.phase == BootstrapPhase::DaemonInstall {
        printer.newline();
        let install_daemon = printer
            .prompt_confirm("Install cfgd daemon for automatic drift detection?")
            .unwrap_or(false);

        if install_daemon {
            match cfgd_core::daemon::install_service(&config_path, Some(&profile_name)) {
                Ok(()) => print_daemon_install_success(printer),
                Err(e) => {
                    printer.warning(&format!("Could not install daemon: {}", e));
                    printer.info("You can install it later with: cfgd daemon --install");
                }
            }
        } else {
            printer.info("Skipped — install later with: cfgd daemon --install");
        }

        state.phase = BootstrapPhase::Complete;
        save_bootstrap_state(&config_dir, &state)?;
    }

    // Offer workflow generation
    let workflow_dir = config_dir.join(".github").join("workflows");
    let workflow_path = workflow_dir.join("cfgd-release.yml");
    if !workflow_path.exists() {
        let generate = printer
            .prompt_confirm("Generate a GitHub Actions release workflow?")
            .unwrap_or(false);
        if generate {
            let profile_names = scan_profile_names(&config_dir.join("profiles"))?;
            let module_names = scan_module_names(&config_dir.join("modules"))?;
            if !profile_names.is_empty() || !module_names.is_empty() {
                let yaml = generate_release_workflow_yaml(&module_names, &profile_names);
                std::fs::create_dir_all(&workflow_dir)?;
                std::fs::write(&workflow_path, &yaml)?;
                printer.success("Generated .github/workflows/cfgd-release.yml");
            }
        }
    }

    // Done
    clear_bootstrap_state(&config_dir);

    printer.newline();
    printer.header("Bootstrap Complete");
    printer.success(&format!("Profile: {}", profile_name));
    printer.success(&format!("Config: {}", config_dir.display()));
    printer.newline();
    printer.info("Useful commands:");
    printer.info("  cfgd status         — view current state");
    printer.info("  cfgd apply --dry-run           — preview changes");
    printer.info("  cfgd apply          — apply changes");
    printer.info("  cfgd daemon         — start drift detection");

    Ok(())
}

fn check_prerequisites(printer: &Printer) -> bool {
    let mut ok = true;

    if !which("git") {
        printer.error("git is not installed — cfgd requires git");

        if cfg!(target_os = "macos") {
            printer.info("Install with: xcode-select --install");
        } else {
            printer.info("Install with: sudo apt install git (or your package manager)");
        }
        ok = false;
    }

    ok
}

fn init_from_remote(printer: &Printer, url: &str, branch: &str) -> anyhow::Result<Option<PathBuf>> {
    // Determine target directory from URL
    let repo_name = url
        .rsplit('/')
        .next()
        .unwrap_or("cfgd-config")
        .trim_end_matches(".git");

    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let target_dir = PathBuf::from(&home).join(format!(".{}", repo_name));

    if target_dir.exists() {
        // Check if it's already a git repo — resumable bootstrap
        if target_dir.join(".git").exists() {
            printer.info(&format!(
                "Repository already exists at {}",
                target_dir.display()
            ));
            printer.info("Pulling latest changes...");

            match cfgd_core::daemon::git_pull_sync(&target_dir) {
                Ok(true) => printer.success("Pulled new changes"),
                Ok(false) => printer.success("Already up to date"),
                Err(e) => printer.warning(&format!(
                    "Pull failed: {} — continuing with existing state",
                    e
                )),
            }

            return Ok(Some(target_dir));
        }

        anyhow::bail!(
            "Directory already exists: {} — remove it or use a different URL",
            target_dir.display()
        );
    }

    // Clone the repository
    printer.info(&format!("Cloning {} (branch: {}) ...", url, branch));

    match cfgd_core::sources::git_clone_with_fallback(url, &target_dir) {
        Ok(()) => {
            printer.success(&format!("Cloned to {}", target_dir.display()));
        }
        Err(e) => {
            anyhow::bail!("Clone failed: {}", e);
        }
    }

    // Checkout the requested branch if not "main"
    if branch != "main" {
        let repo = git2::Repository::open(&target_dir)
            .map_err(|e| anyhow::anyhow!("Failed to open cloned repo: {}", e))?;
        let remote_branch = format!("origin/{}", branch);
        let obj = repo
            .revparse_single(&remote_branch)
            .map_err(|_| anyhow::anyhow!("Branch '{}' not found in remote", branch))?;
        repo.checkout_tree(&obj, None)
            .map_err(|e| anyhow::anyhow!("Failed to checkout branch '{}': {}", branch, e))?;
        repo.set_head(&format!("refs/heads/{}", branch))
            .map_err(|e| anyhow::anyhow!("Failed to set HEAD to '{}': {}", branch, e))?;
        printer.info(&format!("Checked out branch: {}", branch));
    }

    Ok(Some(target_dir))
}

fn init_local(printer: &Printer) -> anyhow::Result<Option<PathBuf>> {
    let config_dir = std::env::current_dir()?;
    let config_path = config_dir.join("cfgd.yaml");

    if config_path.exists() {
        printer.info(&format!(
            "Found existing cfgd.yaml at {}",
            config_dir.display()
        ));
        return Ok(Some(config_dir));
    }

    // Interactive wizard
    printer.subheader("New Configuration");

    let default_name = config_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("my-config")
        .to_string();

    let config_name = printer.prompt_text("Config name", &default_name)?;

    let profiles_dir = config_dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir)?;

    // Profile template selection
    let templates = vec![
        "minimal — packages only".to_string(),
        "standard — packages, files, system".to_string(),
        "empty — blank profile".to_string(),
    ];
    let template_choice = printer.prompt_select("Profile template", &templates)?;

    let profile_content = if template_choice.starts_with("minimal") {
        format!(
            r#"apiVersion: cfgd/v1
kind: Profile
metadata:
  name: default
spec:
  variables:
    EDITOR: "{}"
  packages: {{}}
"#,
            std::env::var("EDITOR").unwrap_or_else(|_| "vim".to_string())
        )
    } else if template_choice.starts_with("standard") {
        format!(
            r#"apiVersion: cfgd/v1
kind: Profile
metadata:
  name: default
spec:
  variables:
    EDITOR: "{}"
  packages: {{}}
  files:
    managed: []
    permissions: {{}}
  system: {{}}
"#,
            std::env::var("EDITOR").unwrap_or_else(|_| "vim".to_string())
        )
    } else {
        r#"apiVersion: cfgd/v1
kind: Profile
metadata:
  name: default
spec:
  variables: {}
  packages: {}
"#
        .to_string()
    };

    let profile_path = profiles_dir.join("default.yaml");
    if !profile_path.exists() {
        std::fs::write(&profile_path, &profile_content)?;
        printer.success("Created profiles/default.yaml");
    }

    // Create cfgd.yaml
    let config_content = format!(
        r#"apiVersion: cfgd/v1
kind: Config
metadata:
  name: {config_name}
spec:
  profile: default
"#
    );
    std::fs::write(&config_path, &config_content)?;
    printer.success("Created cfgd.yaml");

    // Initialize git if not already a repo
    if !config_dir.join(".git").exists() {
        match git2::Repository::init(&config_dir) {
            Ok(_) => printer.success("Initialized git repository"),
            Err(e) => printer.warning(&format!("Could not init git repo: {}", e)),
        }
    }

    // Offer git remote setup
    offer_git_remote_setup(printer, &config_dir)?;

    Ok(Some(config_dir))
}

/// Source-aware init flow for `cfgd init --from` when the cloned repo has cfgd-source.yaml.
fn init_from_source(
    printer: &Printer,
    url: &str,
    source_dir: &Path,
    manifest: cfgd_core::config::ConfigSourceDocument,
) -> anyhow::Result<()> {
    printer.newline();
    printer.subheader("Detected Config Source");
    printer.key_value("Source", &manifest.metadata.name);
    if let Some(ref version) = manifest.metadata.version {
        printer.key_value("Version", version);
    }
    if let Some(ref desc) = manifest.metadata.description {
        printer.key_value("Description", desc);
    }

    let source_name = manifest.metadata.name.clone();
    let provides = &manifest.spec.provides;
    let profile_names = config::source_profile_names(provides);

    if profile_names.is_empty() {
        printer.warning("Source provides no profiles");
        printer.info("Treating as a plain config repo instead");
        // Fall through to normal init (caller already returned if we do)
        return Ok(());
    }

    // Step 3: Platform auto-detection
    let platform = config::detect_platform();
    let platform_display = platform
        .distro
        .as_deref()
        .unwrap_or(&platform.os)
        .to_string();
    let platform_profile_path =
        config::match_platform_profile(&platform, &provides.platform_profiles);
    if let Some(ref path) = platform_profile_path {
        printer.info(&format!(
            "Detected platform: {} -> applying platform profile ({})",
            platform_display, path
        ));
    } else if !provides.platform_profiles.is_empty() {
        printer.info(&format!(
            "No platform profile match for '{}' — skipping platform layer",
            platform_display
        ));
    }

    // Step 4: Profile selection
    printer.newline();
    let selected_profile = if profile_names.len() == 1 {
        printer.info(&format!("One profile available: {}", profile_names[0]));
        profile_names[0].clone()
    } else {
        // Show detailed info if available
        if !provides.profile_details.is_empty() {
            for detail in &provides.profile_details {
                let desc = detail.description.as_deref().unwrap_or("(no description)");
                let inherits = if detail.inherits.is_empty() {
                    String::new()
                } else {
                    format!(" (inherits: {})", detail.inherits.join(", "))
                };
                printer.key_value(&detail.name, &format!("{}{}", desc, inherits));
            }
            printer.newline();
        }

        let selection = printer.prompt_select("Select a profile", &profile_names)?;
        selection.clone()
    };
    printer.success(&format!("Selected profile: {}", selected_profile));

    // Step 5: Policy tier review
    let policy_result = review_policy_tiers(printer, &manifest.spec.policy)?;

    // Step 6: Create local config with source subscription
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let config_dir = PathBuf::from(&home).join(".config").join("cfgd");
    std::fs::create_dir_all(&config_dir)?;

    // Move the source into the proper cache directory
    let cache_dir = cfgd_core::sources::SourceManager::default_cache_dir()?;
    let cached_source_dir = cache_dir.join(&source_name);
    if !cached_source_dir.exists() && source_dir != cached_source_dir {
        std::fs::create_dir_all(&cache_dir)?;
        // Copy (not move) because the clone location may be user-visible
        cfgd_core::copy_dir_recursive(source_dir, &cached_source_dir)?;
    }

    let profiles_dir = config_dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir)?;

    // Create minimal local profile
    let local_profile = r#"apiVersion: cfgd/v1
kind: Profile
metadata:
  name: default
spec:
  variables: {}
  packages: {}
"#;
    let profile_path = profiles_dir.join("default.yaml");
    if !profile_path.exists() {
        std::fs::write(&profile_path, local_profile)?;
    }

    // Build source subscription
    let opt_in_items = policy_result.opt_in.clone();
    let reject_value = if policy_result.rejected.is_empty() {
        String::new()
    } else {
        format!(
            "\n        reject:\n{}",
            policy_result
                .rejected
                .iter()
                .map(|r| format!("          {}: null", r))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };

    let opt_in_section = if opt_in_items.is_empty() {
        String::new()
    } else {
        format!(
            "\n        opt-in:\n{}",
            opt_in_items
                .iter()
                .map(|o| format!("          - {}", o))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };

    let config_content = format!(
        r#"apiVersion: cfgd/v1
kind: Config
metadata:
  name: my-machine
spec:
  profile: default
  sources:
    - name: {source_name}
      origin:
        type: git
        url: {url}
        branch: main
      subscription:
        profile: {selected_profile}
        priority: 500
        accept-recommended: {accept_rec}{opt_in_section}{reject_value}
      sync:
        interval: 1h
        auto-apply: false
"#,
        accept_rec = policy_result.accept_recommended,
    );

    let config_path = config_dir.join("cfgd.yaml");
    std::fs::write(&config_path, &config_content)?;
    printer.success(&format!("Created config at {}", config_path.display()));

    // Initialize git repo for local config
    if !config_dir.join(".git").exists() {
        match git2::Repository::init(&config_dir) {
            Ok(_) => printer.success("Initialized local git repository"),
            Err(e) => printer.warning(&format!("Could not init git repo: {}", e)),
        }
    }

    // Update state store
    let state = open_state_store()?;
    state.upsert_config_source(
        &source_name,
        url,
        "main",
        None,
        manifest.metadata.version.as_deref(),
        None,
    )?;

    // Step 6b: Pre-bootstrap diagnostics
    printer.newline();
    run_pre_bootstrap_diagnostics(printer)?;

    // Step 7: Plan + apply
    printer.newline();
    printer.header("Bootstrap Plan");

    let cfg = config::load_config(&config_path)?;
    let resolved = config::resolve_profile("default", &profiles_dir)?;

    // Compose with the source
    let cache_dir_path = cfgd_core::sources::SourceManager::default_cache_dir()?;
    let mut mgr = SourceManager::new(&cache_dir_path);
    mgr.load_sources(&cfg.spec.sources, printer)?;

    let mut inputs = Vec::new();
    for source_spec in &cfg.spec.sources {
        if let Some(cached) = mgr.get(&source_spec.name) {
            let mut layers = Vec::new();
            if let Some(ref pn) = source_spec.subscription.profile {
                let src_profiles_dir = mgr.source_profiles_dir(&source_spec.name)?;
                if src_profiles_dir.exists() {
                    match config::resolve_profile(pn, &src_profiles_dir) {
                        Ok(r) => layers = r.layers,
                        Err(e) => {
                            printer.warning(&format!(
                                "Failed to resolve source profile '{}': {}",
                                pn, e
                            ));
                        }
                    }
                }
            }
            inputs.push(CompositionInput {
                source_name: source_spec.name.clone(),
                priority: source_spec.subscription.priority,
                policy: cached.manifest.spec.policy.clone(),
                constraints: cached.manifest.spec.policy.constraints.clone(),
                layers,
                subscription: SubscriptionConfig::from_spec(source_spec),
            });
        }
    }

    let composition_result = composition::compose(&resolved, &inputs)?;
    let mut effective = composition_result.resolved;

    // Resolve manifest files
    packages::resolve_manifest_packages(&mut effective.merged.packages, &config_dir)?;

    let registry = build_registry_with_config(Some(&cfg));
    let store = open_state_store()?;
    let reconciler = Reconciler::new(&registry, &store);

    let all_managers: Vec<&dyn cfgd_core::providers::PackageManager> = registry
        .package_managers
        .iter()
        .map(|m| m.as_ref())
        .collect();
    let pkg_actions = packages::plan_packages(&effective.merged, &all_managers)?;

    let fm = CfgdFileManager::new(&config_dir, &effective)?;
    let file_actions = fm.plan(&effective.merged)?;

    let plan = reconciler.plan(&effective, file_actions, pkg_actions, Vec::new())?;

    for phase in &plan.phases {
        let items = reconciler::format_plan_items(phase);
        printer.plan_phase(phase.name.display_name(), &items);
    }

    let total = plan.total_actions();
    printer.newline();
    if total == 0 {
        printer.success("Nothing to do — system already matches desired state");
    } else {
        printer.info(&format!("{} action(s) planned", total));
        printer.newline();

        let confirmed = printer
            .prompt_confirm("Apply these changes?")
            .unwrap_or(false);
        if !confirmed {
            printer.info("Skipped apply. Run 'cfgd apply' when ready.");
            return Ok(());
        }

        // Apply
        printer.newline();
        printer.header("Applying Configuration");

        let cfg2 = config::load_config(&config_path)?;
        let resolved2 = config::resolve_profile("default", &profiles_dir)?;
        let comp2 = composition::compose(&resolved2, &inputs)?;
        let mut eff2 = comp2.resolved;
        packages::resolve_manifest_packages(&mut eff2.merged.packages, &config_dir)?;

        let mut registry2 = build_registry_with_config(Some(&cfg2));
        let all_managers2: Vec<&dyn cfgd_core::providers::PackageManager> = registry2
            .package_managers
            .iter()
            .map(|m| m.as_ref())
            .collect();
        let pkg_actions2 = packages::plan_packages(&eff2.merged, &all_managers2)?;

        let mut fm2 = CfgdFileManager::new(&config_dir, &eff2)?;
        let (backend_name, age_key_path) = secret_backend_from_config(Some(&cfg2));
        fm2.set_secret_providers(
            Some(secrets::build_secret_backend(&backend_name, age_key_path)),
            secrets::build_secret_providers(),
        );
        let file_actions2 = fm2.plan(&eff2.merged)?;
        registry2.file_manager = Some(Box::new(fm2));

        let reconciler2 = Reconciler::new(&registry2, &store);
        let plan2 = reconciler2.plan(&eff2, file_actions2, pkg_actions2, Vec::new())?;
        let result = reconciler2.apply(&plan2, &eff2, &config_dir, printer, None, &[])?;

        printer.newline();
        let status = print_apply_result(&result, printer);
        if status == cfgd_core::state::ApplyStatus::Failed {
            return Ok(());
        }
    }

    // Daemon install offer
    printer.newline();
    let install_daemon = printer
        .prompt_confirm("Install cfgd daemon for continuous sync?")
        .unwrap_or(false);

    if install_daemon {
        let config_path_abs = std::fs::canonicalize(&config_path).unwrap_or(config_path.clone());
        match cfgd_core::daemon::install_service(&config_path_abs, Some("default")) {
            Ok(()) => print_daemon_install_success(printer),
            Err(e) => {
                printer.warning(&format!("Could not install daemon: {}", e));
                printer.info("Install later with: cfgd daemon --install");
            }
        }
    }

    // Summary
    printer.newline();
    printer.header("Bootstrap Complete");
    printer.success(&format!("Source: {} ({})", source_name, url));
    printer.success(&format!("Profile: {}", selected_profile));
    printer.success(&format!("Config: {}", config_dir.display()));
    printer.newline();
    printer.info("Useful commands:");
    printer.info("  cfgd status         — view current state");
    printer.info("  cfgd apply --dry-run           — preview changes");
    printer.info("  cfgd apply          — apply changes");
    printer.info("  cfgd source show    — view source details");

    Ok(())
}

/// Review policy tiers interactively during source-aware init.
struct PolicyReviewResult {
    accept_recommended: bool,
    opt_in: Vec<String>,
    rejected: Vec<String>,
}

fn review_policy_tiers(
    printer: &Printer,
    policy: &config::ConfigSourcePolicy,
) -> anyhow::Result<PolicyReviewResult> {
    printer.newline();
    printer.subheader("Policy Review");

    let required_count = count_policy_items(&policy.required);
    let locked_count = count_policy_items(&policy.locked);
    let recommended_count = count_policy_items(&policy.recommended);
    let optional_profiles = &policy.optional.profiles;

    // Show required + locked (mandatory, no prompt)
    if locked_count > 0 || required_count > 0 {
        printer.newline();
        printer.info("Required (always applied):");
        if locked_count > 0 {
            display_policy_items(printer, &policy.locked, "  ");
        }
        if required_count > 0 {
            display_policy_items(printer, &policy.required, "  ");
        }
    }

    // Prompt for recommended (default yes)
    let accept_recommended = if recommended_count > 0 {
        printer.newline();
        printer.info("Recommended:");
        display_policy_items(printer, &policy.recommended, "  ");
        printer.newline();
        printer
            .prompt_confirm_with_default("Accept recommended items?", true)
            .unwrap_or(true)
    } else {
        false
    };

    // Prompt for optional profiles (default no each)
    let mut opt_in = Vec::new();
    if !optional_profiles.is_empty() {
        printer.newline();
        printer.info("Optional profiles:");
        for profile in optional_profiles {
            printer.info(&format!("  {}", profile));
        }
        printer.newline();
        for profile in optional_profiles {
            let accepted = printer
                .prompt_confirm_with_default(&format!("Opt in to '{}'?", profile), false)
                .unwrap_or(false);
            if accepted {
                opt_in.push(profile.clone());
            }
        }
    }

    Ok(PolicyReviewResult {
        accept_recommended,
        opt_in,
        rejected: Vec::new(),
    })
}

/// Offer to set up a git remote for the config repo.
fn offer_git_remote_setup(printer: &Printer, config_dir: &Path) -> anyhow::Result<()> {
    // Check if remote already exists
    if let Ok(repo) = git2::Repository::open(config_dir)
        && repo.find_remote("origin").is_ok()
    {
        return Ok(());
    }

    let setup = printer
        .prompt_confirm_with_default("Set up a git remote for this config repo?", false)
        .unwrap_or(false);
    if !setup {
        return Ok(());
    }

    let options = vec![
        "Enter URL manually".to_string(),
        "I'll set it up later".to_string(),
    ];

    // Offer gh repo create as an option if gh is available
    let has_gh = which("gh");
    let options = if has_gh {
        vec![
            "Create with gh (GitHub CLI)".to_string(),
            "Enter URL manually".to_string(),
            "I'll set it up later".to_string(),
        ]
    } else {
        options
    };

    let choice = printer.prompt_select("How to set up the remote?", &options)?;

    if choice.starts_with("Create with gh") {
        // Print the command for the user — we can't shell out from cli/ per the architecture rules
        let repo_name = config_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("cfgd-config");
        printer.newline();
        printer.info("Run this command to create and push:");
        printer.info(&format!(
            "  gh repo create {} --private --source=. --push",
            repo_name
        ));
    } else if choice.starts_with("Enter URL") {
        let url = printer.prompt_text("Remote URL (git@... or https://...)", "")?;
        if !url.is_empty() {
            match git2::Repository::open(config_dir) {
                Ok(repo) => match repo.remote("origin", &url) {
                    Ok(_) => printer.success(&format!("Added remote 'origin' -> {}", url)),
                    Err(e) => printer.warning(&format!("Could not add remote: {}", e)),
                },
                Err(e) => printer.warning(&format!("Could not open repo: {}", e)),
            }
        }
    }

    Ok(())
}

/// Run quick pre-bootstrap diagnostics before plan/apply.
fn run_pre_bootstrap_diagnostics(printer: &Printer) -> anyhow::Result<()> {
    printer.subheader("Pre-Bootstrap Diagnostics");

    let mut all_ok = true;

    // Git
    if which("git") {
        printer.success("git: found");
    } else {
        printer.error("git: not found — required for cfgd");
        all_ok = false;
    }

    // Package manager availability
    let registry = build_registry();
    let mut shown = std::collections::HashSet::new();
    for mgr in &registry.package_managers {
        let name = mgr.name();
        if name == "brew-tap" || name == "brew-cask" {
            continue;
        }
        if !shown.insert(name.to_string()) {
            continue;
        }
        if mgr.is_available() {
            printer.success(&format!("{}: available", name));
        } else if mgr.can_bootstrap() {
            printer.info(&format!(
                "{}: not found — will be auto-bootstrapped if needed",
                name
            ));
        }
    }

    // State store
    match StateStore::open_default() {
        Ok(_) => printer.success("State store: accessible"),
        Err(e) => {
            printer.warning(&format!("State store: {}", e));
            all_ok = false;
        }
    }

    if !all_ok {
        printer.newline();
        printer.warning("Some checks failed — bootstrap may encounter issues");
    }

    Ok(())
}

fn bootstrap_profile_select(
    config_dir: &Path,
    printer: &Printer,
) -> anyhow::Result<Option<String>> {
    let profiles_dir = config_dir.join("profiles");

    if !profiles_dir.exists() {
        printer.warning("No profiles directory found");
        return Ok(None);
    }

    let mut profiles = Vec::new();
    for entry in std::fs::read_dir(&profiles_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("yaml")
            && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
        {
            profiles.push(stem.to_string());
        }
    }

    if profiles.is_empty() {
        printer.warning("No profile files found in profiles/");
        return Ok(None);
    }

    profiles.sort();

    let selected = if profiles.len() == 1 {
        let name = &profiles[0];
        printer.info(&format!("Found one profile: {}", name));
        name.clone()
    } else {
        printer.info(&format!("Found {} profiles:", profiles.len()));

        // Show profile summaries
        for name in &profiles {
            let path = profiles_dir.join(format!("{}.yaml", name));
            if let Ok(doc) = config::load_profile(&path) {
                let pkg_count = count_packages(&doc.spec);
                let file_count = doc
                    .spec
                    .files
                    .as_ref()
                    .map(|f| f.managed.len())
                    .unwrap_or(0);
                let inherits = if doc.spec.inherits.is_empty() {
                    String::new()
                } else {
                    format!(" (inherits: {})", doc.spec.inherits.join(", "))
                };
                printer.key_value(
                    name,
                    &format!("{} packages, {} files{}", pkg_count, file_count, inherits),
                );
            }
        }

        printer.newline();
        match printer.prompt_select("Select a profile", &profiles) {
            Ok(selected) => selected.clone(),
            Err(_) => {
                printer.info("No profile selected — aborted");
                return Ok(None);
            }
        }
    };

    printer.success(&format!("Selected profile: {}", selected));
    Ok(Some(selected))
}

fn count_packages(spec: &config::ProfileSpec) -> usize {
    let mut count = 0;
    if let Some(ref pkgs) = spec.packages {
        if let Some(ref brew) = pkgs.brew {
            count += brew.formulae.len() + brew.casks.len();
        }
        if let Some(ref apt) = pkgs.apt {
            count += apt.packages.len();
        }
        if let Some(ref cargo) = pkgs.cargo {
            count += cargo.packages.len();
        }
        if let Some(ref npm) = pkgs.npm {
            count += npm.global.len();
        }
        count += pkgs.pipx.len();
        count += pkgs.dnf.len();
    }
    count
}

fn bootstrap_secrets_setup(config_dir: &Path, printer: &Printer) -> anyhow::Result<()> {
    printer.newline();
    printer.subheader("Secrets Setup");

    let health = secrets::check_secrets_health(config_dir, None);

    if health.sops_available {
        let version_str = health.sops_version.as_deref().unwrap_or("unknown version");
        printer.success(&format!("sops: found ({})", version_str));
    } else {
        printer.info("sops: not installed (optional — required for secret management)");
        printer.info("  Install: https://github.com/getsops/sops#install");
    }

    if health.age_key_exists {
        if let Some(ref path) = health.age_key_path {
            printer.success(&format!("age key: {}", path.display()));
        }
    } else if health.sops_available {
        // Offer to generate age key
        let generate = printer
            .prompt_confirm("Generate age encryption key for secrets?")
            .unwrap_or(false);

        if generate {
            match secrets::init_age_key(config_dir) {
                Ok(key_path) => {
                    printer.success(&format!("Age key generated: {}", key_path.display()));
                }
                Err(e) => {
                    printer.warning(&format!("Could not generate age key: {}", e));
                    printer.info("Generate later with: cfgd secret init");
                }
            }
        } else {
            printer.info("Skipped — generate later with: cfgd secret init");
        }
    }

    // Check for external secret providers
    for (name, available) in &health.providers {
        if *available {
            printer.success(&format!("provider {}: available", name));
        }
    }

    Ok(())
}

fn ensure_config_file(
    config_dir: &Path,
    config_path: &Path,
    profile_name: &str,
    from_url: Option<&str>,
    branch: &str,
    theme: Option<&str>,
) -> anyhow::Result<()> {
    if config_path.exists() {
        // Update profile in existing config
        let contents = std::fs::read_to_string(config_path)?;
        let mut cfg = config::parse_config(&contents, config_path)?;
        if cfg.spec.profile != profile_name {
            cfg.spec.profile = profile_name.to_string();
            let yaml = serde_yaml::to_string(&cfg)?;
            std::fs::write(config_path, &yaml)?;
        }
        return Ok(());
    }

    // Generate new cfgd.yaml
    let name = config_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("my-config");

    let origin_section = if let Some(url) = from_url {
        format!(
            r#"  origin:
    type: git
    url: {}
    branch: {}
"#,
            url, branch
        )
    } else {
        String::new()
    };

    let theme_section = if let Some(preset) = theme {
        format!(
            r#"  theme:
    preset: {}
"#,
            preset
        )
    } else {
        String::new()
    };

    let config_content = format!(
        r#"apiVersion: cfgd/v1
kind: Config
metadata:
  name: {}
spec:
  profile: {}
{}{}"#,
        name, profile_name, origin_section, theme_section
    );

    std::fs::write(config_path, &config_content)?;
    Ok(())
}
/// 1. Validates arguments
/// 2. Exchanges bootstrap token for permanent device credential
/// 3. Saves credential locally
/// 4. Saves any desired config pushed by server
/// 5. Prints next steps
pub(super) fn cmd_init_server(
    printer: &Printer,
    server_url: Option<&str>,
    token: Option<&str>,
) -> anyhow::Result<()> {
    printer.header("Server Enrollment");

    let server_url = match server_url {
        Some(url) => url,
        None => {
            anyhow::bail!(
                "--server is required for server enrollment\nUsage: cfgd init --server <url> --token <bootstrap-token>"
            );
        }
    };

    let token = match token {
        Some(t) => t,
        None => {
            anyhow::bail!(
                "--token is required for server enrollment\nGet a bootstrap token from your team admin"
            );
        }
    };

    let device_id = default_device_id();
    printer.key_value("Server", server_url);
    printer.key_value("Device ID", &device_id);
    printer.newline();

    // Create a client with no auth (enrollment doesn't need pre-auth)
    let client = cfgd_core::server_client::ServerClient::new(server_url, None, &device_id);

    // Enroll
    printer.info("Exchanging bootstrap token for device credential...");
    let resp = client
        .enroll(token, printer)
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    printer.newline();
    printer.success(&format!("Enrolled as user '{}'", resp.username));
    if let Some(ref team) = resp.team {
        printer.key_value("Team", team);
    }
    printer.key_value("Device", &resp.device_id);

    // Save credential
    let credential = cfgd_core::server_client::DeviceCredential {
        server_url: server_url.to_string(),
        device_id: resp.device_id.clone(),
        api_key: resp.api_key.clone(),
        username: resp.username.clone(),
        team: resp.team.clone(),
        enrolled_at: cfgd_core::utc_now_iso8601(),
    };

    match cfgd_core::server_client::save_credential(&credential) {
        Ok(path) => {
            printer.success(&format!("Credential saved to {}", path.display()));
        }
        Err(e) => {
            printer.error(&format!("Failed to save credential: {}", e));
            printer.warning("You will need to manually provide --api-key for future commands");
        }
    }

    // Save desired config if server pushed one
    if let Some(ref desired) = resp.desired_config {
        match cfgd_core::state::save_pending_server_config(desired) {
            Ok(path) => {
                printer.newline();
                printer.info(&format!(
                    "Server pushed desired config — saved to {}",
                    path.display()
                ));
                printer.info(MSG_RUN_APPLY);
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to save pending server config");
            }
        }
    }

    printer.newline();
    printer.header("Next Steps");
    printer.info("  cfgd checkin --server-url <url>   — report status to server");
    printer.info("  cfgd apply --dry-run                         — preview configuration");
    printer.info("  cfgd apply                        — apply configuration");
    printer.info("  cfgd daemon --install              — start background sync");

    Ok(())
}

pub(super) fn cmd_init_module(
    printer: &Printer,
    url: &str,
    module_name: &str,
) -> anyhow::Result<()> {
    printer.header("Module Bootstrap");

    if !check_prerequisites(printer) {
        return Ok(());
    }

    // Clone the repo
    let cloned_dir = init_from_remote(printer, url, "main")?;
    let config_dir = match cloned_dir {
        Some(dir) => dir,
        None => return Ok(()),
    };

    // Verify the module exists in the cloned repo
    let all_modules = modules::load_modules(&config_dir)?;
    if !all_modules.contains_key(module_name) {
        printer.error(&format!("Module '{}' not found in repository", module_name));
        if !all_modules.is_empty() {
            let mut available: Vec<&str> = all_modules.keys().map(|s| s.as_str()).collect();
            available.sort();
            printer.info(&format!("Available modules: {}", available.join(", ")));
        }
        return Ok(());
    }

    // Create minimal cfgd.yaml with just this module
    let config_path = config_dir.join("cfgd.yaml");
    if !config_path.exists() {
        let profiles_dir = config_dir.join("profiles");
        std::fs::create_dir_all(&profiles_dir)?;

        let profile_content = format!(
            r#"apiVersion: cfgd/v1
kind: Profile
metadata:
  name: default
spec:
  modules:
    - {}
  variables: {{}}
  packages: {{}}
"#,
            module_name
        );
        std::fs::write(profiles_dir.join("default.yaml"), &profile_content)?;

        let config_name = config_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("cfgd-config");
        let config_content = format!(
            r#"apiVersion: cfgd/v1
kind: Config
metadata:
  name: {}
spec:
  profile: default
"#,
            config_name.trim_start_matches('.')
        );
        std::fs::write(&config_path, &config_content)?;
        printer.success("Created minimal cfgd.yaml with module profile");
    }

    // Resolve module and its dependencies
    let platform = Platform::detect();
    printer.key_value(
        "Platform",
        &format!("{}/{}/{}", platform.os, platform.distro, platform.arch),
    );

    let registry = build_registry();
    let mgr_map = managers_map(&registry);
    let cache_base = modules::default_module_cache_dir()?;

    let module_names = vec![module_name.to_string()];
    let resolved_modules =
        modules::resolve_modules(&module_names, &config_dir, &cache_base, &platform, &mgr_map)?;

    if resolved_modules.is_empty() {
        printer.warning("No actions resolved for this module");
        return Ok(());
    }

    // Show plan
    printer.newline();
    printer.subheader("Module Plan");
    for rm in &resolved_modules {
        printer.info(&format!(
            "  {} ({} packages, {} files)",
            rm.name,
            rm.packages.len(),
            rm.files.len()
        ));
        for pkg in &rm.packages {
            let ver = pkg.version.as_deref().unwrap_or("-");
            printer.info(&format!(
                "    + {} install {} ({})",
                pkg.manager, pkg.resolved_name, ver
            ));
        }
        for file in &rm.files {
            printer.info(&format!("    -> {}", file.target.display()));
        }
    }

    // Confirm
    printer.newline();
    let confirmed = printer
        .prompt_confirm("Apply this module?")
        .unwrap_or(false);
    if !confirmed {
        printer.info("Aborted");
        return Ok(());
    }

    // Apply via reconciler
    let resolved = config::resolve_profile("default", &config_dir.join("profiles"))?;
    let state = open_state_store()?;
    let reconciler = Reconciler::new(&registry, &state);
    let plan = reconciler.plan(&resolved, Vec::new(), Vec::new(), resolved_modules.clone())?;
    let result = reconciler.apply(
        &plan,
        &resolved,
        &config_dir,
        printer,
        None,
        &resolved_modules,
    )?;

    printer.newline();
    print_apply_result(&result, printer);

    printer.newline();
    printer.info("Useful commands:");
    printer.info("  cfgd module show <name>  — view module details");
    printer.info("  cfgd apply --dry-run               — preview all changes");
    printer.info("  cfgd apply              — apply changes");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_printer() -> cfgd_core::output::Printer {
        cfgd_core::output::Printer::new(cfgd_core::output::Verbosity::Quiet)
    }

    #[test]
    fn bootstrap_state_serialization_roundtrip() {
        let dir = tempfile::tempdir().unwrap();

        let state = BootstrapState {
            repo_url: Some("https://github.com/test/bootstrap.git".to_string()),
            config_dir: "/home/user/.config/cfgd".to_string(),
            profile: Some("work".to_string()),
            phase: BootstrapPhase::Apply,
        };

        save_bootstrap_state(dir.path(), &state).unwrap();
        let loaded = load_bootstrap_state(dir.path()).unwrap();

        assert_eq!(loaded.repo_url, state.repo_url);
        assert_eq!(loaded.config_dir, state.config_dir);
        assert_eq!(loaded.profile, state.profile);
        assert_eq!(loaded.phase, BootstrapPhase::Apply);
    }

    #[test]
    fn bootstrap_state_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_bootstrap_state(dir.path()).is_none());
    }

    #[test]
    fn clear_bootstrap_state_removes_file() {
        let dir = tempfile::tempdir().unwrap();

        let state = BootstrapState {
            repo_url: None,
            config_dir: ".".to_string(),
            profile: None,
            phase: BootstrapPhase::Clone,
        };

        save_bootstrap_state(dir.path(), &state).unwrap();
        assert!(dir.path().join(BOOTSTRAP_STATE_FILE).exists());

        clear_bootstrap_state(dir.path());
        assert!(!dir.path().join(BOOTSTRAP_STATE_FILE).exists());
    }

    #[test]
    fn ensure_config_file_creates_new() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("cfgd.yaml");

        ensure_config_file(
            dir.path(),
            &config_path,
            "work",
            Some("https://github.com/test/init-cfg.git"),
            "main",
            None,
        )
        .unwrap();

        assert!(config_path.exists());
        let contents = std::fs::read_to_string(&config_path).unwrap();
        assert!(contents.contains("profile: work"));
        assert!(contents.contains("https://github.com/test/init-cfg.git"));
    }

    #[test]
    fn ensure_config_file_updates_existing() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("cfgd.yaml");

        std::fs::write(
            &config_path,
            r#"apiVersion: cfgd/v1
kind: Config
metadata:
  name: test
spec:
  profile: default
"#,
        )
        .unwrap();

        ensure_config_file(dir.path(), &config_path, "work", None, "main", None).unwrap();

        let cfg = config::load_config(&config_path).unwrap();
        assert_eq!(cfg.spec.profile, "work");
    }

    #[test]
    fn ensure_config_file_no_update_if_same_profile() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("cfgd.yaml");

        let original = r#"apiVersion: cfgd/v1
kind: Config
metadata:
  name: test
spec:
  profile: default
"#;
        std::fs::write(&config_path, original).unwrap();

        ensure_config_file(dir.path(), &config_path, "default", None, "main", None).unwrap();

        // Should not be rewritten
        let contents = std::fs::read_to_string(&config_path).unwrap();
        assert_eq!(contents, original);
    }

    #[test]
    fn ensure_config_file_with_branch_and_theme() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("cfgd.yaml");

        ensure_config_file(
            dir.path(),
            &config_path,
            "work",
            Some("https://github.com/test/cfg.git"),
            "develop",
            Some("minimal"),
        )
        .unwrap();

        let contents = std::fs::read_to_string(&config_path).unwrap();
        assert!(contents.contains("branch: develop"));
        assert!(contents.contains("preset: minimal"));
    }

    #[test]
    fn count_packages_empty() {
        let spec = config::ProfileSpec::default();
        assert_eq!(count_packages(&spec), 0);
    }

    #[test]
    fn count_packages_with_various() {
        let spec = config::ProfileSpec {
            packages: Some(config::PackagesSpec {
                brew: Some(config::BrewSpec {
                    formulae: vec!["rg".into(), "fd".into()],
                    casks: vec!["firefox".into()],
                    ..Default::default()
                }),
                cargo: Some(config::CargoSpec {
                    file: None,
                    packages: vec!["bat".into()],
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(count_packages(&spec), 4);
    }

    #[test]
    fn init_local_returns_existing_config() {
        let dir = tempfile::tempdir().unwrap();
        let original_dir = std::env::current_dir().unwrap();

        // Pre-create cfgd.yaml so init_local takes the fast path (no prompts)
        let config_path = dir.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd/v1\nkind: Config\nmetadata:\n  name: test\nspec:\n  profile: default\n",
        )
        .unwrap();

        std::env::set_current_dir(dir.path()).unwrap();

        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        let result = init_local(&printer).unwrap();

        std::env::set_current_dir(&original_dir).unwrap();

        assert!(result.is_some());
        let config_dir = result.unwrap();
        assert!(config_dir.join("cfgd.yaml").exists());
    }

    #[test]
    fn bootstrap_profile_select_single_profile() {
        let dir = tempfile::tempdir().unwrap();
        let profiles_dir = dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();

        std::fs::write(
            profiles_dir.join("default.yaml"),
            r#"apiVersion: cfgd/v1
kind: Profile
metadata:
  name: default
spec:
  variables: {}
"#,
        )
        .unwrap();

        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        let result = bootstrap_profile_select(dir.path(), &printer).unwrap();
        assert_eq!(result, Some("default".to_string()));
    }

    #[test]
    fn bootstrap_phase_display_names() {
        assert_eq!(BootstrapPhase::Clone.display_name(), "Clone repository");
        assert_eq!(BootstrapPhase::Apply.display_name(), "Apply configuration");
        assert_eq!(BootstrapPhase::Complete.display_name(), "Complete");
    }

    #[test]
    fn profile_switch_via_config_update() {
        let dir = tempfile::tempdir().unwrap();
        let profiles_dir = dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(
            profiles_dir.join("work.yaml"),
            "apiVersion: cfgd/v1\nkind: Profile\nmetadata:\n  name: work\nspec:\n  variables: {}\n",
        )
        .unwrap();

        // Create cfgd.yaml
        let config_path = dir.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            r#"apiVersion: cfgd/v1
kind: Config
metadata:
  name: test
spec:
  profile: default
"#,
        )
        .unwrap();

        // Simulate what cmd_profile_switch does: update profile in cfgd.yaml
        ensure_config_file(dir.path(), &config_path, "work", None, "main", None).unwrap();

        let cfg = config::load_config(&config_path).unwrap();
        assert_eq!(cfg.spec.profile, "work");
    }
}
