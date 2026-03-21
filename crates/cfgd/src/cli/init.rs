use std::path::Path;

use super::*;

// ─────────────────────────────────────────────────────
// cfgd init — pure scaffolding
// ─────────────────────────────────────────────────────

pub(super) struct InitArgs<'a> {
    pub path: Option<&'a str>,
    pub from: Option<&'a str>,
    pub branch: &'a str,
    pub name: Option<&'a str>,
    pub apply: bool,
    pub dry_run: bool,
    pub yes: bool,
    pub install_daemon: bool,
    pub theme: Option<&'a str>,
    pub apply_profile: Option<&'a str>,
    pub apply_modules: &'a [String],
}

/// Scaffold a new cfgd configuration repository.
pub(super) fn cmd_init(printer: &Printer, args: &InitArgs<'_>) -> anyhow::Result<()> {
    printer.header("Initialize cfgd");

    if !check_prerequisites(printer) {
        return Ok(());
    }

    // 1. Determine target directory
    let target_dir = match args.path {
        Some(p) => cfgd_core::expand_tilde(Path::new(p)),
        None => std::env::current_dir()?,
    };

    // 2. Create directory if it doesn't exist
    if !target_dir.exists() {
        std::fs::create_dir_all(&target_dir)?;
    }

    // 3. Check if already initialized
    if target_dir.join("cfgd.yaml").exists() {
        printer.info(&format!("Already initialized at {}", target_dir.display()));
        return Ok(());
    }

    // 4. Clone or scaffold
    if let Some(url) = args.from {
        clone_into(&target_dir, url, args.branch, printer)?;
        // If --theme was specified and the cloned repo has a cfgd.yaml, set the theme
        if let Some(theme) = args.theme {
            let config_path = target_dir.join("cfgd.yaml");
            if config_path.exists() {
                let mut cfg = config::load_config(&config_path)?;
                cfg.spec.theme = Some(config::ThemeConfig {
                    name: theme.to_string(),
                    overrides: config::ThemeOverrides::default(),
                });
                let yaml = serde_yaml::to_string(&cfg)?;
                cfgd_core::atomic_write_str(&config_path, &yaml)?;
            }
        }
    } else {
        scaffold(&target_dir, args.name, args.theme, printer)?;
    }

    // 5. Generate release workflow based on what's present
    regenerate_workflow(&target_dir, printer)?;

    // 6. Git init if not already a repo
    if !target_dir.join(".git").exists() {
        match git2::Repository::init(&target_dir) {
            Ok(_) => printer.success("Initialized git repository"),
            Err(e) => printer.warning(&format!("Could not init git repo: {}", e)),
        }
    }

    printer.newline();
    printer.success(&format!("Initialized at {}", target_dir.display()));

    // 7. Apply if requested
    let should_apply = args.apply || args.apply_profile.is_some() || !args.apply_modules.is_empty();
    if should_apply {
        let config_path = target_dir.join("cfgd.yaml");
        let profiles_dir = target_dir.join("profiles");

        // Module-only apply: no profile needed
        let module_only = !args.apply_modules.is_empty() && args.apply_profile.is_none();

        if module_only {
            // Validate that requested modules exist
            let cache_base = modules::default_module_cache_dir()?;
            let all_modules = modules::load_all_modules(&target_dir, &cache_base)?;
            for m in args.apply_modules {
                let resolved_name = modules::resolve_profile_module_name(m);
                if !all_modules.contains_key(resolved_name) {
                    anyhow::bail!("Module '{}' not found in {}", m, target_dir.display());
                }
            }

            printer.newline();
            printer.header("Applying Modules");

            let cfg = config::load_config(&config_path)?;
            let registry = super::build_registry_with_config(Some(&cfg));
            let store = super::open_state_store(None)?;

            // Build a minimal resolved profile for the reconciler
            let resolved = config::ResolvedProfile {
                layers: Vec::new(),
                merged: config::MergedProfile::default(),
            };

            let platform = cfgd_core::platform::Platform::detect();
            let mgr_map = super::managers_map(&registry);
            let resolved_modules = modules::resolve_modules(
                args.apply_modules,
                &target_dir,
                &cache_base,
                &platform,
                &mgr_map,
            )?;

            let reconciler = cfgd_core::reconciler::Reconciler::new(&registry, &store);
            let plan = reconciler.plan(&resolved, Vec::new(), Vec::new(), resolved_modules)?;

            apply_plan(
                &plan,
                &reconciler,
                &resolved,
                &target_dir,
                args.dry_run,
                args.yes,
                printer,
            )?;
        } else {
            // Profile-based apply
            let profile_name = if let Some(name) = args.apply_profile {
                // Validate profile exists
                let profile_path = profiles_dir.join(format!("{}.yaml", name));
                if !profile_path.exists() {
                    anyhow::bail!("Profile '{}' not found at {}", name, profile_path.display());
                }
                // Set as active profile in cfgd.yaml
                let mut cfg = config::load_config(&config_path)?;
                cfg.spec.profile = Some(name.to_string());
                let yaml = serde_yaml::to_string(&cfg)?;
                cfgd_core::atomic_write_str(&config_path, &yaml)?;
                printer.success(&format!("Set active profile: {}", name));
                name.to_string()
            } else {
                // No --apply-profile: use whatever's in cfgd.yaml, or pick interactively
                let cfg = config::load_config(&config_path)?;
                if let Some(ref p) = cfg.spec.profile {
                    p.clone()
                } else {
                    pick_profile(&profiles_dir, printer)?
                }
            };

            printer.newline();
            printer.header("Applying Configuration");

            let cfg = config::load_config(&config_path)?;
            let resolved = config::resolve_profile(&profile_name, &profiles_dir)?;
            let mut registry = super::build_registry_with_config(Some(&cfg));
            let store = super::open_state_store(None)?;

            // Resolve modules (profile modules + any --apply-module additions)
            let mut module_names = resolved.merged.modules.clone();
            for m in args.apply_modules {
                if !module_names.contains(m) {
                    module_names.push(m.clone());
                }
            }

            let resolved_modules = if !module_names.is_empty() {
                let platform = cfgd_core::platform::Platform::detect();
                let mgr_map = super::managers_map(&registry);
                let cache_base = modules::default_module_cache_dir()?;
                // Validate --apply-module names exist
                for m in args.apply_modules {
                    let cache_base = modules::default_module_cache_dir()?;
                    let all_modules = modules::load_all_modules(&target_dir, &cache_base)?;
                    let resolved_name = modules::resolve_profile_module_name(m);
                    if !all_modules.contains_key(resolved_name) {
                        anyhow::bail!("Module '{}' not found in {}", m, target_dir.display());
                    }
                }
                modules::resolve_modules(
                    &module_names,
                    &target_dir,
                    &cache_base,
                    &platform,
                    &mgr_map,
                )?
            } else {
                Vec::new()
            };

            let all_managers: Vec<&dyn cfgd_core::providers::PackageManager> = registry
                .package_managers
                .iter()
                .map(|m| m.as_ref())
                .collect();
            let pkg_actions = super::packages::plan_packages(&resolved.merged, &all_managers)?;

            let fm = super::CfgdFileManager::new(&target_dir, &resolved)?;
            let file_actions = fm.plan(&resolved.merged)?;

            registry.file_manager = Some(Box::new(fm));

            let reconciler = cfgd_core::reconciler::Reconciler::new(&registry, &store);
            let plan = reconciler.plan(&resolved, file_actions, pkg_actions, resolved_modules)?;

            apply_plan(
                &plan,
                &reconciler,
                &resolved,
                &target_dir,
                args.dry_run,
                args.yes,
                printer,
            )?;
        }
    }

    // 8. Install daemon if requested
    if args.install_daemon {
        let config_path = target_dir.join("cfgd.yaml");
        let cfg = config::load_config(&config_path)?;
        let profile = cfg.spec.profile.as_deref();
        match cfgd_core::daemon::install_service(&config_path, profile) {
            Ok(()) => printer.success("Daemon service installed"),
            Err(e) => {
                printer.warning(&format!("Could not install daemon: {}", e));
                printer.info("Install later with: cfgd daemon install");
            }
        }
    }

    // 9. Print next steps
    if !args.apply {
        printer.newline();
        printer.info("Next steps:");
        printer.info("  cfgd module create <name>   — create a module");
        printer.info("  cfgd profile create <name>  — create a profile");
        printer.info("  cfgd apply                  — apply configuration");
    }

    Ok(())
}

/// Show plan, prompt for confirmation, and apply.
fn apply_plan(
    plan: &cfgd_core::reconciler::Plan,
    reconciler: &cfgd_core::reconciler::Reconciler<'_>,
    resolved: &config::ResolvedProfile,
    config_dir: &Path,
    dry_run: bool,
    yes: bool,
    printer: &Printer,
) -> anyhow::Result<()> {
    let total = plan.total_actions();
    if total == 0 {
        printer.success("Nothing to do — system is already configured");
        return Ok(());
    }

    for phase in &plan.phases {
        let items = cfgd_core::reconciler::format_plan_items(phase);
        printer.plan_phase(phase.name.display_name(), &items);
    }
    printer.info(&format!("{} action(s) planned", total));

    if dry_run {
        return Ok(());
    }

    if !yes {
        let confirmed = printer
            .prompt_confirm("Apply these changes?")
            .unwrap_or(false);
        if !confirmed {
            printer.info("Skipped — run 'cfgd apply' to apply later");
            return Ok(());
        }
    }

    let state_dir = cfgd_core::state::default_state_dir()
        .map_err(|e| anyhow::anyhow!("cannot determine state directory: {}", e))?;
    let _apply_lock = cfgd_core::acquire_apply_lock(&state_dir)?;

    let result = reconciler.apply(plan, resolved, config_dir, printer, None, &[])?;
    super::print_apply_result(&result, printer);
    Ok(())
}

/// Interactively pick a profile from the profiles directory.
fn pick_profile(profiles_dir: &Path, printer: &Printer) -> anyhow::Result<String> {
    if !profiles_dir.is_dir() {
        anyhow::bail!(
            "No profiles directory found — create a profile first with: cfgd profile create <name>"
        );
    }

    let names = super::list_yaml_stems(profiles_dir)?;

    if names.is_empty() {
        anyhow::bail!(
            "No profiles found — create a profile first with: cfgd profile create <name>"
        );
    }

    if names.len() == 1 {
        printer.info(&format!("Using only available profile: {}", names[0]));
        return Ok(names[0].clone());
    }

    printer.subheader("Available Profiles");
    for (i, name) in names.iter().enumerate() {
        printer.info(&format!("  {}. {}", i + 1, name));
    }

    let input = printer.prompt_text(&format!("Select profile (1-{}) or name", names.len()), "1")?;

    // Try as number first
    if let Ok(n) = input.parse::<usize>()
        && n >= 1
        && n <= names.len()
    {
        return Ok(names[n - 1].clone());
    }

    // Try as name
    if names.contains(&input) {
        return Ok(input);
    }

    anyhow::bail!(
        "Invalid selection '{}' — expected a number or profile name",
        input
    )
}

/// Clone a remote repo into the target directory.
fn clone_into(target_dir: &Path, url: &str, branch: &str, printer: &Printer) -> anyhow::Result<()> {
    // If target already has .git, pull instead
    if target_dir.join(".git").exists() {
        printer.info("Repository already exists, pulling latest...");
        match cfgd_core::daemon::git_pull_sync(target_dir) {
            Ok(true) => printer.success("Pulled new changes"),
            Ok(false) => printer.success("Already up to date"),
            Err(e) => printer.warning(&format!("Pull failed: {} — continuing", e)),
        }
        return Ok(());
    }

    printer.info(&format!("Cloning {} (branch: {}) ...", url, branch));

    cfgd_core::sources::git_clone_with_fallback(url, target_dir)
        .map_err(|e| anyhow::anyhow!("Clone failed: {}", e))?;

    printer.success(&format!("Cloned to {}", target_dir.display()));

    // Checkout branch if not main
    if branch != "master" {
        let repo = git2::Repository::open(target_dir)
            .map_err(|e| anyhow::anyhow!("Failed to open cloned repo: {}", e))?;
        let remote_branch = format!("origin/{}", branch);
        let obj = repo
            .revparse_single(&remote_branch)
            .map_err(|_| anyhow::anyhow!("Branch '{}' not found in remote", branch))?;
        repo.checkout_tree(&obj, None)
            .map_err(|e| anyhow::anyhow!("Failed to checkout '{}': {}", branch, e))?;
        repo.set_head(&format!("refs/heads/{}", branch))
            .map_err(|e| anyhow::anyhow!("Failed to set HEAD to '{}': {}", branch, e))?;
        printer.info(&format!("Checked out branch: {}", branch));
    }

    Ok(())
}

/// Create the cfgd directory structure from scratch.
fn scaffold(
    dir: &Path,
    name: Option<&str>,
    theme: Option<&str>,
    printer: &Printer,
) -> anyhow::Result<()> {
    let config_name = name
        .or_else(|| dir.file_name().and_then(|n| n.to_str()))
        .unwrap_or("my-config");

    // Create directories
    std::fs::create_dir_all(dir.join("profiles"))?;
    std::fs::create_dir_all(dir.join("modules"))?;
    printer.success("Created profiles/ modules/");

    // cfgd.yaml
    let theme_value = theme.unwrap_or("default");
    let content = format!(
        r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: {config_name}
spec:
  theme: {theme_value}
  fileStrategy: Symlink
  aliases:
    add: "profile update --file"
    remove: "profile update --file"
  # profile: base
  # modules:
  #   registries: []
  # sources: []
"#
    );
    std::fs::write(dir.join("cfgd.yaml"), &content)?;
    printer.success("Created cfgd.yaml");

    // .gitignore — ignore everything except cfgd-managed content
    let gitignore = "\
# Ignore everything by default
**

# cfgd config
!cfgd.yaml
!.gitignore
!README.md

# Profiles and modules
!profiles/
!profiles/**
!modules/
!modules/**

# CI
!.github/
!.github/**
";
    std::fs::write(dir.join(".gitignore"), gitignore)?;
    printer.success("Created .gitignore");

    // README.md
    let readme = format!(
        r#"# {config_name}

Machine configuration managed by [cfgd](https://github.com/tj-smith47/cfgd).

## Quick start

```bash
cfgd init --from <this-repo-url>
cfgd apply
```

## Structure

- `profiles/` — machine profiles (which modules, packages, env, system settings to apply)
- `modules/` — self-contained configuration units (packages, files, env, scripts)
- `cfgd.yaml` — config root (active profile, sources, theme)
"#
    );
    std::fs::write(dir.join("README.md"), &readme)?;
    printer.success("Created README.md");

    // Workflow — generate a base workflow even with no modules/profiles yet.
    // It gets regenerated when modules/profiles are added.
    let workflow_dir = dir.join(".github").join("workflows");
    std::fs::create_dir_all(&workflow_dir)?;
    let workflow = generate_release_workflow_yaml(&[], &[]);
    std::fs::write(workflow_dir.join("cfgd-release.yml"), &workflow)?;
    printer.success("Created .github/workflows/cfgd-release.yml");

    Ok(())
}

/// Generate or regenerate the release workflow based on current modules/profiles.
/// Called by init and also by module create / profile create.
pub(super) fn regenerate_workflow(config_dir: &Path, printer: &Printer) -> anyhow::Result<()> {
    let profiles = scan_profile_names(&config_dir.join("profiles"))?;
    let modules = scan_module_names(&config_dir.join("modules"))?;

    if profiles.is_empty() && modules.is_empty() {
        return Ok(());
    }

    let workflow_dir = config_dir.join(".github").join("workflows");
    std::fs::create_dir_all(&workflow_dir)?;

    let yaml = generate_release_workflow_yaml(&modules, &profiles);
    std::fs::write(workflow_dir.join("cfgd-release.yml"), &yaml)?;
    printer.success("Generated .github/workflows/cfgd-release.yml");

    Ok(())
}

fn check_prerequisites(printer: &Printer) -> bool {
    if !cfgd_core::command_available("git") {
        printer.error("git is not installed — cfgd requires git");
        if cfg!(target_os = "macos") {
            printer.info("Install with: xcode-select --install");
        } else {
            printer.info("Install with: sudo apt install git (or your package manager)");
        }
        return false;
    }
    true
}

// ─────────────────────────────────────────────────────
// ensure_config_file — used by profile switch and tests
// ─────────────────────────────────────────────────────

#[cfg(test)]
fn ensure_config_file(
    config_dir: &Path,
    config_path: &Path,
    profile_name: &str,
    from_url: Option<&str>,
    branch: &str,
    theme: Option<&str>,
) -> anyhow::Result<()> {
    if config_path.exists() {
        let contents = std::fs::read_to_string(config_path)?;
        let mut cfg = config::parse_config(&contents, config_path)?;
        if cfg.spec.profile.as_deref() != Some(profile_name) {
            cfg.spec.profile = Some(profile_name.to_string());
            let yaml = serde_yaml::to_string(&cfg)?;
            std::fs::write(config_path, &yaml)?;
        }
        return Ok(());
    }

    let name = config_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("my-config");

    let origin_section = if let Some(url) = from_url {
        format!(
            r#"  origin:
    type: Git
    url: {}
    branch: {}
"#,
            url, branch
        )
    } else {
        String::new()
    };

    let theme_value = theme.unwrap_or("default");

    let config_content = format!(
        r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: {}
spec:
  profile: {}
  theme: {}
  fileStrategy: Symlink
{}"#,
        name, profile_name, theme_value, origin_section
    );

    std::fs::write(config_path, &config_content)?;
    Ok(())
}

// ─────────────────────────────────────────────────────
// count_packages — used by profile display
// ─────────────────────────────────────────────────────

#[cfg(test)]
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

// ─────────────────────────────────────────────────────
// cfgd enroll — unified enrollment (token + key-based)
// ─────────────────────────────────────────────────────

pub(super) fn cmd_enroll(
    printer: &Printer,
    server_url: &str,
    token: Option<&str>,
    ssh_key: Option<&str>,
    gpg_key: Option<&str>,
    username: Option<&str>,
) -> anyhow::Result<()> {
    let username = match username {
        Some(u) => u.to_string(),
        None => std::env::var("USER").unwrap_or_else(|_| "unknown".to_string()),
    };
    let device_id = default_device_id();

    printer.key_value("Server", server_url);
    printer.key_value("Device ID", &device_id);

    let client = cfgd_core::server_client::ServerClient::new(server_url, None, &device_id);

    // Token-based enrollment (direct exchange)
    if let Some(token) = token {
        printer.header("Token Enrollment");
        printer.info("Exchanging bootstrap token for device credential...");

        let resp = client
            .enroll(token, printer)
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        return finish_enrollment(printer, server_url, &device_id, resp);
    }

    // Key-based enrollment (challenge-response)
    printer.header("Key-Based Enrollment");
    printer.key_value("Username", &username);

    // Check server enrollment method
    let info = client.enroll_info().map_err(|e| anyhow::anyhow!("{}", e))?;

    if info.method == "token" {
        anyhow::bail!(
            "This server uses bootstrap token enrollment. Run: cfgd enroll --server-url <url> --token <token>"
        );
    }

    // Determine signing method
    let (key_type, key_ref) = if let Some(gpg_id) = gpg_key {
        ("gpg".to_string(), gpg_id.to_string())
    } else if let Some(ssh_path) = ssh_key {
        ("ssh".to_string(), ssh_path.to_string())
    } else {
        match detect_ssh_key(printer) {
            Some(path) => ("ssh".to_string(), path),
            None => {
                anyhow::bail!(
                    "no SSH key found — provide --ssh-key <path> or --gpg-key <id>\n\
                     Checked: SSH agent, ~/.ssh/id_ed25519, ~/.ssh/id_rsa, ~/.ssh/id_ecdsa"
                );
            }
        }
    };

    printer.key_value(
        "Signing with",
        &format!("{} ({})", key_type.to_uppercase(), key_ref),
    );
    printer.newline();

    // Challenge-response
    let challenge = client
        .request_challenge(&username, printer)
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    printer.key_value("Challenge ID", &challenge.challenge_id);
    printer.key_value("Expires", &challenge.expires_at);

    let signature = match key_type.as_str() {
        "ssh" => sign_with_ssh(&challenge.nonce, &key_ref)?,
        "gpg" => sign_with_gpg(&challenge.nonce, &key_ref)?,
        _ => unreachable!(),
    };

    printer.success("Challenge signed");

    let resp = client
        .submit_verification(&challenge.challenge_id, &signature, &key_type, printer)
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    finish_enrollment(printer, server_url, &device_id, resp)
}

/// Shared enrollment completion: save credential, handle desired config, print next steps.
fn finish_enrollment(
    printer: &Printer,
    server_url: &str,
    device_id: &str,
    resp: cfgd_core::server_client::EnrollResponse,
) -> anyhow::Result<()> {
    printer.newline();
    printer.success(&format!("Enrolled as user '{}'", resp.username));
    if let Some(ref team) = resp.team {
        printer.key_value("Team", team);
    }
    printer.key_value("Device", &resp.device_id);

    let credential = cfgd_core::server_client::DeviceCredential {
        server_url: server_url.to_string(),
        device_id: device_id.to_string(),
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
    printer.info("  cfgd checkin --server-url <url>  — report status to server");
    printer.info("  cfgd apply --dry-run             — preview configuration");
    printer.info("  cfgd apply                       — apply configuration");
    printer.info("  cfgd daemon install               — start background sync");

    Ok(())
}

// ─────────────────────────────────────────────────────
// SSH/GPG signing helpers
// ─────────────────────────────────────────────────────

fn detect_ssh_key(printer: &Printer) -> Option<String> {
    // Try SSH agent first
    if let Ok(output) = std::process::Command::new("ssh-add").arg("-l").output()
        && output.status.success()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Some(line) = stdout.lines().next()
            && !line.contains("no identities")
        {
            let home = std::env::var("HOME").unwrap_or_default();
            for key_name in &["id_ed25519", "id_rsa", "id_ecdsa"] {
                let pub_path = Path::new(&home)
                    .join(".ssh")
                    .join(format!("{key_name}.pub"));
                if pub_path.exists() {
                    printer.info(&format!("Using SSH key from agent: {}", pub_path.display()));
                    return Some(pub_path.to_string_lossy().to_string());
                }
            }
            for key_name in &["id_ed25519", "id_rsa", "id_ecdsa"] {
                let key_path = Path::new(&home).join(".ssh").join(key_name);
                if key_path.exists() {
                    printer.info(&format!("Using SSH key: {}", key_path.display()));
                    return Some(key_path.to_string_lossy().to_string());
                }
            }
        }
    }

    // Fall back to on-disk keys
    let home = std::env::var("HOME").unwrap_or_default();
    for key_name in &["id_ed25519", "id_rsa", "id_ecdsa"] {
        let pub_path = Path::new(&home)
            .join(".ssh")
            .join(format!("{key_name}.pub"));
        if pub_path.exists() {
            printer.info(&format!("Using SSH key: {}", pub_path.display()));
            return Some(pub_path.to_string_lossy().to_string());
        }
        let key_path = Path::new(&home).join(".ssh").join(key_name);
        if key_path.exists() {
            printer.info(&format!("Using SSH key: {}", key_path.display()));
            return Some(key_path.to_string_lossy().to_string());
        }
    }

    None
}

fn sign_with_ssh(nonce: &str, key_path: &str) -> anyhow::Result<String> {
    let tmp_dir = tempfile::tempdir()?;
    let data_path = tmp_dir.path().join("challenge.txt");
    let sig_path = tmp_dir.path().join("challenge.txt.sig");

    std::fs::write(&data_path, nonce)?;

    let status = std::process::Command::new("ssh-keygen")
        .args([
            "-Y",
            "sign",
            "-f",
            key_path,
            "-n",
            "cfgd-enroll",
            data_path.to_str().unwrap_or("challenge.txt"),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .status()
        .map_err(|e| anyhow::anyhow!("ssh-keygen not found: {e} — is OpenSSH installed?"))?;

    if !status.success() {
        anyhow::bail!("ssh-keygen -Y sign failed — check that your SSH key is accessible");
    }

    let signature = std::fs::read_to_string(&sig_path)
        .map_err(|e| anyhow::anyhow!("failed to read SSH signature: {e}"))?;

    Ok(signature)
}

fn sign_with_gpg(nonce: &str, gpg_key_id: &str) -> anyhow::Result<String> {
    let tmp_dir = tempfile::tempdir()?;
    let data_path = tmp_dir.path().join("challenge.txt");
    let sig_path = tmp_dir.path().join("challenge.txt.asc");

    std::fs::write(&data_path, nonce)?;

    let status = std::process::Command::new("gpg")
        .args([
            "--batch",
            "--yes",
            "--detach-sign",
            "--armor",
            "-u",
            gpg_key_id,
            "-o",
            sig_path.to_str().unwrap_or("challenge.txt.asc"),
            data_path.to_str().unwrap_or("challenge.txt"),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .status()
        .map_err(|e| anyhow::anyhow!("gpg not found: {e} — is GPG installed?"))?;

    if !status.success() {
        anyhow::bail!(
            "gpg --detach-sign failed — check that key '{}' is available",
            gpg_key_id
        );
    }

    let signature = std::fs::read_to_string(&sig_path)
        .map_err(|e| anyhow::anyhow!("failed to read GPG signature: {e}"))?;

    Ok(signature)
}

// ─────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaffold_creates_structure() {
        let dir = tempfile::tempdir().unwrap();
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);

        scaffold(dir.path(), Some("test-config"), None, &printer).unwrap();

        assert!(dir.path().join("cfgd.yaml").exists());
        assert!(dir.path().join("profiles").is_dir());
        assert!(dir.path().join("modules").is_dir());
        assert!(!dir.path().join("files").exists());
        assert!(dir.path().join(".gitignore").exists());

        let contents = std::fs::read_to_string(dir.path().join("cfgd.yaml")).unwrap();
        assert!(contents.contains("name: test-config"));
    }

    #[test]
    fn scaffold_with_theme() {
        let dir = tempfile::tempdir().unwrap();
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);

        scaffold(dir.path(), Some("themed"), Some("minimal"), &printer).unwrap();

        let contents = std::fs::read_to_string(dir.path().join("cfgd.yaml")).unwrap();
        assert!(contents.contains("name: themed"));
        assert!(contents.contains("theme: minimal"));
    }

    #[test]
    fn scaffold_uses_dir_name_as_default() {
        let dir = tempfile::tempdir().unwrap();
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);

        scaffold(dir.path(), None, None, &printer).unwrap();

        let contents = std::fs::read_to_string(dir.path().join("cfgd.yaml")).unwrap();
        // Should use the tempdir name
        assert!(contents.contains("name:"));
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
            "master",
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
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: test\nspec:\n  profile: default\n",
        )
        .unwrap();

        ensure_config_file(dir.path(), &config_path, "work", None, "master", None).unwrap();

        let cfg = config::load_config(&config_path).unwrap();
        assert_eq!(cfg.spec.profile.as_deref(), Some("work"));
    }

    #[test]
    fn ensure_config_file_no_update_if_same_profile() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("cfgd.yaml");

        let original = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: test\nspec:\n  profile: default\n";
        std::fs::write(&config_path, original).unwrap();

        ensure_config_file(dir.path(), &config_path, "default", None, "master", None).unwrap();

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
        assert!(contents.contains("theme: minimal"));
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
    fn profile_switch_via_config_update() {
        let dir = tempfile::tempdir().unwrap();
        let profiles_dir = dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(
            profiles_dir.join("work.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: work\nspec:\n  env: []\n",
        )
        .unwrap();

        let config_path = dir.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: test\nspec:\n  profile: default\n",
        )
        .unwrap();

        ensure_config_file(dir.path(), &config_path, "work", None, "master", None).unwrap();

        let cfg = config::load_config(&config_path).unwrap();
        assert_eq!(cfg.spec.profile.as_deref(), Some("work"));
    }

    #[test]
    fn regenerate_workflow_empty_repo() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("profiles")).unwrap();
        std::fs::create_dir_all(dir.path().join("modules")).unwrap();

        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        regenerate_workflow(dir.path(), &printer).unwrap();

        // No profiles or modules → no workflow generated
        assert!(
            !dir.path()
                .join(".github/workflows/cfgd-release.yml")
                .exists()
        );
    }

    #[test]
    fn regenerate_workflow_with_profile() {
        let dir = tempfile::tempdir().unwrap();
        let profiles_dir = dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::create_dir_all(dir.path().join("modules")).unwrap();
        std::fs::write(
            profiles_dir.join("base.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: base\nspec: {}\n",
        )
        .unwrap();

        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        regenerate_workflow(dir.path(), &printer).unwrap();

        let workflow = dir.path().join(".github/workflows/cfgd-release.yml");
        assert!(workflow.exists());
        let contents = std::fs::read_to_string(&workflow).unwrap();
        assert!(contents.contains("base"));
    }

    #[test]
    fn scaffold_includes_default_theme() {
        let dir = tempfile::tempdir().unwrap();
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);

        scaffold(dir.path(), Some("test"), None, &printer).unwrap();
        let cfg = config::load_config(&dir.path().join("cfgd.yaml")).unwrap();
        assert_eq!(cfg.spec.theme.unwrap().name, "default");
    }

    #[test]
    fn scaffold_with_custom_theme() {
        let dir = tempfile::tempdir().unwrap();
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);

        scaffold(dir.path(), Some("test"), Some("minimal"), &printer).unwrap();
        let cfg = config::load_config(&dir.path().join("cfgd.yaml")).unwrap();
        assert_eq!(cfg.spec.theme.unwrap().name, "minimal");
    }

    #[test]
    fn pick_profile_single_profile() {
        let dir = tempfile::tempdir().unwrap();
        let profiles_dir = dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(
            profiles_dir.join("base.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: base\nspec: {}\n",
        )
        .unwrap();

        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        let result = pick_profile(&profiles_dir, &printer).unwrap();
        assert_eq!(result, "base");
    }

    #[test]
    fn pick_profile_no_profiles_errors() {
        let dir = tempfile::tempdir().unwrap();
        let profiles_dir = dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();

        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        let result = pick_profile(&profiles_dir, &printer);
        assert!(result.is_err());
    }

    #[test]
    fn pick_profile_no_dir_errors() {
        let dir = tempfile::tempdir().unwrap();
        let profiles_dir = dir.path().join("profiles");
        // Don't create the dir

        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        let result = pick_profile(&profiles_dir, &printer);
        assert!(result.is_err());
    }
}
