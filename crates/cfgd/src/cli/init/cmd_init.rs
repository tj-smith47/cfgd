use std::path::Path;

use cfgd_core::output::{Doc, Printer, Role};
use serde::Serialize;

use super::source::{clone_into, is_git_source, resolve_from};
use super::*;

// ─────────────────────────────────────────────────────
// cfgd init — pure scaffolding
// ─────────────────────────────────────────────────────

pub struct InitArgs<'a> {
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

/// Structured-output payload for `cfgd init`. Drives `-o json|yaml|jsonpath|template`.
#[derive(Debug, Serialize)]
pub(crate) struct InitOutput {
    pub(crate) target_dir: String,
}

/// Scaffold a new cfgd configuration repository.
pub fn cmd_init(printer: &Printer, args: &InitArgs<'_>) -> anyhow::Result<()> {
    printer.heading("Initialize cfgd");

    if !check_prerequisites(printer) {
        let output = InitOutput {
            target_dir: args.path.unwrap_or("").to_string(),
        };
        printer.emit(Doc::new().with_data(&output));
        return Ok(());
    }

    // 1. Determine target directory and whether --from did a fresh clone
    let from_used = args.from.is_some();
    let target_dir = if let Some(from) = args.from {
        let explicit_path = args.path.map(|p| cfgd_core::expand_tilde(Path::new(p)));
        resolve_from(from, explicit_path.as_deref(), args.branch, printer)?
    } else {
        match args.path {
            Some(p) => cfgd_core::expand_tilde(Path::new(p)),
            None => std::env::current_dir()?,
        }
    };

    // 2. Create directory if it doesn't exist
    if !target_dir.exists() {
        std::fs::create_dir_all(&target_dir)?;
    }

    // 3. Check if already initialized
    // When --from is used, resolve_from handles the "already initialized" case
    // and the clone creates cfgd.yaml — skip this check so we reach the apply step
    if target_dir.join("cfgd.yaml").exists() && !from_used {
        printer.status_simple(
            Role::Info,
            format!("Already initialized at {}", target_dir.display()),
        );
        let output = InitOutput {
            target_dir: target_dir.display().to_string(),
        };
        printer.emit(Doc::new().with_data(&output));
        return Ok(());
    }

    // 4. Clone or scaffold
    // When --from is a git source, resolve_from already cloned it above.
    // Only clone here if resolve_from didn't handle it (non-git --from or no --from).
    if let Some(url) = args.from.filter(|f| is_git_source(f)) {
        if !target_dir.join(".git").exists() {
            clone_into(&target_dir, url, args.branch, printer)?;
        }
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

    // 5. Generate release workflow — only for scaffolded repos, not cloned ones.
    // Cloned repos already have their own workflows; writing into them dirties the tree.
    if !from_used {
        regenerate_workflow(&target_dir, printer)?;
    }

    // 6. Git init if not already a repo
    if !target_dir.join(".git").exists() {
        match git2::Repository::init(&target_dir) {
            Ok(_) => printer.status_simple(Role::Ok, "Initialized git repository"),
            Err(e) => printer.status_simple(Role::Warn, format!("Failed to init git repo: {}", e)),
        }
    }

    printer.status_simple(Role::Ok, format!("Initialized at {}", target_dir.display()));

    // 7. Apply if requested
    let should_apply = should_run_apply(args.apply, args.apply_profile, args.apply_modules);
    if should_apply {
        let config_path = target_dir.join("cfgd.yaml");
        let profiles_dir = target_dir.join("profiles");

        // Module-only apply: no profile needed
        let module_only = is_module_only_apply(args.apply_profile, args.apply_modules);

        if module_only {
            // Validate that requested modules exist
            let cache_base = modules::default_module_cache_dir()?;
            let all_modules = modules::load_all_modules(&target_dir, &cache_base, printer)?;
            for m in args.apply_modules {
                let resolved_name = modules::resolve_profile_module_name(m);
                if !all_modules.contains_key(resolved_name) {
                    anyhow::bail!("Module '{}' not found in {}", m, target_dir.display());
                }
            }

            printer.heading("Applying Modules");

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
                printer,
            )?;

            let reconciler = cfgd_core::reconciler::Reconciler::new(&registry, &store);
            let plan = reconciler.plan(
                &resolved,
                Vec::new(),
                Vec::new(),
                resolved_modules,
                cfgd_core::reconciler::ReconcileContext::Apply,
            )?;

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
                printer.status_simple(Role::Ok, format!("Set active profile: {}", name));
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

            printer.heading("Applying Configuration");

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
                // Validate --apply-module names exist (load once, check all)
                let all_modules = modules::load_all_modules(&target_dir, &cache_base, printer)?;
                for m in args.apply_modules {
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
                    printer,
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
            let plan = reconciler.plan(
                &resolved,
                file_actions,
                pkg_actions,
                resolved_modules,
                cfgd_core::reconciler::ReconcileContext::Apply,
            )?;

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
        #[cfg(any(unix, windows))]
        {
            let config_path = target_dir.join("cfgd.yaml");
            let cfg = config::load_config(&config_path)?;
            let profile = cfg.spec.profile.as_deref();
            match cfgd_core::daemon::install_service(&config_path, profile) {
                Ok(()) => {
                    printer.status_simple(Role::Ok, "Daemon service installed");
                    #[cfg(windows)]
                    printer
                        .status_simple(Role::Info, "The service will start automatically on boot");
                }
                Err(e) => {
                    printer.status_simple(Role::Warn, format!("Failed to install daemon: {}", e));
                    printer.hint("Install later with: cfgd daemon install");
                }
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            printer.status_simple(
                Role::Warn,
                "Daemon service installation is not supported on this platform",
            );
            printer.hint("Run the daemon directly with: cfgd daemon");
        }
    }

    // 9. Print next steps (and always emit a structured-output anchor so
    // `-o json` consumers receive the target_dir payload regardless of path).
    // The "Next steps" section is suppressed whenever an apply ran — the
    // apply branch already produced its own report.
    let output = InitOutput {
        target_dir: target_dir.display().to_string(),
    };
    let doc = if !should_apply {
        Doc::new()
            .section("Next steps", |s| {
                s.bullet("cfgd module create <name>   — create a module")
                    .bullet("cfgd profile create <name>  — create a profile")
                    .bullet("cfgd apply                  — apply configuration")
            })
            .with_data(&output)
    } else {
        Doc::new().with_data(&output)
    };
    printer.emit(doc);

    Ok(())
}

/// Decide whether `cfgd init` should run an apply step after scaffolding.
///
/// Returns `true` when any of:
/// - `--apply` was passed explicitly,
/// - `--apply-profile <name>` was passed (apply against that profile), or
/// - `--apply-module <m>...` named at least one module.
///
/// Pure helper — split out so the precedence rules are testable without
/// running the orchestration around it.
pub(super) fn should_run_apply(
    apply: bool,
    apply_profile: Option<&str>,
    apply_modules: &[String],
) -> bool {
    apply || apply_profile.is_some() || !apply_modules.is_empty()
}

/// Decide whether the apply step should run in module-only mode (no profile
/// resolution). True iff `--apply-module` named at least one module *and*
/// no `--apply-profile` was supplied — modules can be applied standalone
/// without a profile, but as soon as a profile is named the regular
/// profile-based apply path takes over.
pub(super) fn is_module_only_apply(apply_profile: Option<&str>, apply_modules: &[String]) -> bool {
    !apply_modules.is_empty() && apply_profile.is_none()
}

/// Show plan, prompt for confirmation, and apply.
pub(super) fn apply_plan(
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
        printer.status_simple(Role::Ok, "Nothing to do — system is already configured");
        return Ok(());
    }

    super::display_plan_table(plan, printer, None);
    printer.status_simple(Role::Info, format!("{} action(s) planned", total));

    if dry_run {
        return Ok(());
    }

    if !yes {
        let confirmed = printer
            .prompt_confirm("Apply these changes?")
            .unwrap_or(false);
        if !confirmed {
            printer.status_simple(Role::Info, "Skipped — run 'cfgd apply' to apply later");
            return Ok(());
        }
    }

    let state_dir = cfgd_core::state::default_state_dir()
        .map_err(|e| anyhow::anyhow!("cannot determine state directory: {}", e))?;
    let _apply_lock = cfgd_core::acquire_apply_lock(&state_dir)?;

    let result = reconciler.apply(
        plan,
        resolved,
        config_dir,
        printer,
        None,
        &[],
        cfgd_core::reconciler::ReconcileContext::Apply,
        false,
    )?;
    super::print_apply_result(&result, printer, None);
    Ok(())
}

/// Interactively pick a profile from the profiles directory.
pub(super) fn pick_profile(profiles_dir: &Path, printer: &Printer) -> anyhow::Result<String> {
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
        printer.status_simple(
            Role::Info,
            format!("Using only available profile: {}", names[0]),
        );
        return Ok(names[0].clone());
    }

    let section = printer.section("Available Profiles");
    for (i, name) in names.iter().enumerate() {
        section.bullet(format!("{}. {}", i + 1, name));
    }
    drop(section);

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

/// Create the cfgd directory structure from scratch.
pub(super) fn scaffold(
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
    printer.status_simple(Role::Ok, "Created profiles/ modules/");

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
    cfgd_core::atomic_write_str(&dir.join("cfgd.yaml"), &content)?;
    printer.status_simple(Role::Ok, "Created cfgd.yaml");

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
    cfgd_core::atomic_write_str(&dir.join(".gitignore"), gitignore)?;
    printer.status_simple(Role::Ok, "Created .gitignore");

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
    cfgd_core::atomic_write_str(&dir.join("README.md"), &readme)?;
    printer.status_simple(Role::Ok, "Created README.md");

    // Workflow — generate a base workflow even with no modules/profiles yet.
    // It gets regenerated when modules/profiles are added.
    let workflow_dir = dir.join(".github").join("workflows");
    std::fs::create_dir_all(&workflow_dir)?;
    let default_branch =
        cfgd_core::detect_default_branch(dir).unwrap_or_else(|| "master".to_string());
    let workflow = generate_release_workflow_yaml(&[], &[], &default_branch);
    cfgd_core::atomic_write_str(&workflow_dir.join("cfgd-release.yml"), &workflow)?;
    printer.status_simple(Role::Ok, "Created .github/workflows/cfgd-release.yml");

    Ok(())
}

/// Generate or regenerate the release workflow based on current modules/profiles.
/// Called by init and also by module create / profile create.
pub(crate) fn regenerate_workflow(config_dir: &Path, printer: &Printer) -> anyhow::Result<()> {
    let profiles = scan_profile_names(&config_dir.join("profiles"))?;
    let modules = scan_module_names(&config_dir.join("modules"))?;

    if profiles.is_empty() && modules.is_empty() {
        return Ok(());
    }

    let workflow_dir = config_dir.join(".github").join("workflows");
    std::fs::create_dir_all(&workflow_dir)?;

    let default_branch =
        cfgd_core::detect_default_branch(config_dir).unwrap_or_else(|| "master".to_string());
    let yaml = generate_release_workflow_yaml(&modules, &profiles, &default_branch);
    cfgd_core::atomic_write_str(&workflow_dir.join("cfgd-release.yml"), &yaml)?;
    printer.status_simple(Role::Ok, "Generated .github/workflows/cfgd-release.yml");

    Ok(())
}

pub(super) fn check_prerequisites(printer: &Printer) -> bool {
    if !cfgd_core::command_available("git") {
        printer.status_simple(Role::Fail, "git is not installed — cfgd requires git");
        if cfg!(target_os = "macos") {
            printer.hint("Install with: xcode-select --install");
        } else {
            printer.hint("Install with: sudo apt install git (or your package manager)");
        }
        return false;
    }
    true
}
