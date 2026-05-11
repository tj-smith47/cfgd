use super::*;

pub(crate) fn cmd_module_add_from_registry(
    cli: &Cli,
    printer: &Printer,
    reference: &str,
    yes: bool,
    allow_unsigned: bool,
) -> anyhow::Result<()> {
    let reg_ref = modules::parse_registry_ref(reference).ok_or_else(|| {
        anyhow::anyhow!(
            "Invalid registry reference '{}' — expected registry/module[@tag]",
            reference
        )
    })?;

    // Load config to find the registry
    if !cli.config.exists() {
        anyhow::bail!("{}", MSG_NO_CONFIG);
    }
    let cfg = config::load_config(&cli.config)?;

    let registries = cfg
        .spec
        .modules
        .as_ref()
        .map(|m| &m.registries[..])
        .unwrap_or(&[]);
    let registry_entry = registries
        .iter()
        .find(|s| s.name == reg_ref.registry)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Registry '{}' not configured — run 'cfgd module registry add <url>' first",
                reg_ref.registry
            )
        })?;

    // Resolve the tag — use explicit tag or find latest
    let cache_base = modules::default_module_cache_dir()?;
    let tag = match reg_ref.tag {
        Some(t) => t,
        None => {
            printer.info(&format!(
                "No tag specified — looking up latest for '{}'",
                reg_ref.module
            ));
            // Fetch the registry repo so we can read tags
            modules::fetch_registry_modules(registry_entry, &cache_base, printer)?;
            modules::latest_module_version(registry_entry, &reg_ref.module, &cache_base)?
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "No tags found for module '{}' in registry '{}'",
                        reg_ref.module,
                        reg_ref.registry
                    )
                })?
        }
    };

    // Build the full git URL with subdir and delegate to cmd_module_add_remote
    let full_url = build_registry_module_url(&registry_entry.url, &reg_ref.module, &tag);
    printer.info(&format!(
        "Resolved: {}/{} -> {}",
        reg_ref.registry, reg_ref.module, full_url
    ));

    cmd_module_add_remote(
        cli,
        printer,
        &full_url,
        Some(&reg_ref.registry),
        yes,
        allow_unsigned,
    )
}

pub(crate) fn cmd_module_add_remote(
    cli: &Cli,
    printer: &Printer,
    url: &str,
    source_name: Option<&str>,
    yes: bool,
    allow_unsigned: bool,
) -> anyhow::Result<()> {
    printer.header("Add Remote Module");

    let config_dir = config_dir(cli);
    let cache_base = modules::default_module_cache_dir()?;

    // Fetch the remote module (validates pinned ref)
    printer.info(&format!("Fetching {}", url));
    let fetched = modules::fetch_remote_module(url, &cache_base, printer)?;
    let module_name = fetched.module.name.clone();
    let module = fetched.module;
    let commit = fetched.commit;
    let integrity = fetched.integrity;

    // Check if already in lockfile
    let mut lockfile = modules::load_lockfile(&config_dir)?;
    if lockfile.modules.iter().any(|e| e.name == module_name) {
        printer.info(&format!(
            "Module '{}' is already in the lockfile — use 'cfgd module update {}' to change versions",
            module_name, module_name
        ));
        return Ok(());
    }

    // Check if a local module with the same name exists
    let local_modules = modules::load_modules(&config_dir)?;
    if local_modules.contains_key(&module_name) {
        anyhow::bail!(
            "Local module '{}' already exists in {}/modules/ — local modules take precedence over remote",
            module_name,
            config_dir.display()
        );
    }

    print_module_review_summary(printer, &module_name, &module, &commit, &integrity);

    // Check for GPG/SSH signature on the tag
    let git_src = modules::parse_git_source(url)?;
    enforce_signature_policy(
        cli,
        printer,
        git_src.tag.as_deref(),
        &module_name,
        allow_unsigned,
        &cache_base,
        &git_src.repo_url,
    )?;

    // Confirm
    printer.newline();
    if !yes && !printer.prompt_confirm("Add this remote module?")? {
        printer.info("Cancelled");
        return Ok(());
    }

    // Build clean lockfile URL: repo@tag (no //subdir — subdir is a separate field)
    let lock_url = compute_lock_url(url, &git_src);

    let pinned_ref = compute_pinned_ref(&git_src, &commit);

    let lock_entry = cfgd_core::config::ModuleLockEntry {
        name: module_name.clone(),
        url: lock_url,
        pinned_ref,
        commit,
        integrity,
        subdir: git_src.subdir,
    };
    lockfile.modules.push(lock_entry);
    modules::save_lockfile(&config_dir, &lockfile)?;

    // Add to profile if config exists — use registry/module format for registry-backed modules
    let profile_module_ref = match source_name {
        Some(src) => format!("{}/{}", src, module_name),
        None => module_name.clone(),
    };
    if cli.config.exists() {
        let cfg = config::load_config(&cli.config)?;
        let profile_name = match cli.profile.as_deref() {
            Some(p) => p,
            None => cfg.active_profile()?,
        };
        let profile_path = profiles_dir(cli).join(format!("{}.yaml", profile_name));

        if profile_path.exists() {
            let contents = std::fs::read_to_string(&profile_path)?;
            let mut doc: config::ProfileDocument = serde_yaml::from_str(&contents)?;

            if ensure_module_in_profile_doc(&mut doc, &profile_module_ref) {
                let yaml = serde_yaml::to_string(&doc)?;
                cfgd_core::atomic_write_str(&profile_path, &yaml)?;
                printer.success(&format!(
                    "Added module '{}' to profile '{}'",
                    profile_module_ref, profile_name
                ));
            }
        }
    }

    printer.success(&format!(
        "Locked remote module '{}' in modules.lock",
        module_name
    ));
    printer.info(MSG_RUN_APPLY);

    Ok(())
}

pub(crate) fn cmd_module_upgrade(
    cli: &Cli,
    printer: &Printer,
    name: &str,
    new_ref: Option<&str>,
    yes: bool,
    allow_unsigned: bool,
) -> anyhow::Result<()> {
    printer.header(&format!("Update Module: {}", name));

    let config_dir = config_dir(cli);
    let cache_base = modules::default_module_cache_dir()?;

    // Find the lockfile entry
    let mut lockfile = modules::load_lockfile(&config_dir)?;
    let entry_idx = lockfile.modules.iter().position(|e| e.name == name);

    let entry_idx = match entry_idx {
        Some(idx) => idx,
        None => {
            let local_modules = modules::load_modules(&config_dir)?;
            if local_modules.contains_key(name) {
                anyhow::bail!(
                    "Module '{}' is a local module — edit it directly in {}/modules/{}/",
                    name,
                    config_dir.display(),
                    name
                );
            } else {
                anyhow::bail!("Module '{}' not found", name);
            }
        }
    };

    let old_entry = lockfile.modules[entry_idx].clone();

    // Load the current module for diffing
    let old_git_src = modules::parse_git_source(&old_entry.url)?;
    let old_pinned_src = modules::GitSource {
        repo_url: old_git_src.repo_url.clone(),
        tag: Some(old_entry.pinned_ref.clone()),
        git_ref: None,
        subdir: old_entry.subdir.clone(),
    };
    let old_local_path = modules::fetch_git_source(&old_pinned_src, &cache_base, name, printer)?;
    let old_module = modules::load_module(&old_local_path)?;

    // Build the new URL with the updated ref
    let new_ref = match new_ref {
        Some(r) => r.to_string(),
        None => {
            // Fetch latest from remote and use HEAD
            let git_src = modules::GitSource {
                repo_url: old_git_src.repo_url.clone(),
                tag: None,
                git_ref: None,
                subdir: None,
            };
            modules::fetch_git_source(&git_src, &cache_base, name, printer)?;
            let repo_dir = modules::git_cache_dir(&cache_base, &old_git_src.repo_url);
            let head = modules::get_head_commit_sha(&repo_dir)?;
            printer.info(&format!("Latest commit: {}", head));
            head
        }
    };

    // Fetch the new version
    let new_src = modules::GitSource {
        repo_url: old_git_src.repo_url.clone(),
        tag: Some(new_ref.clone()),
        git_ref: None,
        subdir: old_entry.subdir.clone(),
    };
    let new_local_path = modules::fetch_git_source(&new_src, &cache_base, name, printer)?;
    let new_module = modules::load_module(&new_local_path)?;
    let repo_dir = modules::git_cache_dir(&cache_base, &old_git_src.repo_url);
    let new_commit = modules::get_head_commit_sha(&repo_dir)?;
    let new_integrity = modules::hash_module_contents(&new_local_path)?;

    if new_commit == old_entry.commit {
        printer.info("Module is already at this version");
        return Ok(());
    }

    // Show diff
    printer.subheader("Changes");
    let changes = modules::diff_module_specs(&old_module, &new_module);
    for change in &changes {
        printer.info(&format!("  {}", change));
    }

    printer.newline();
    printer.key_value("Old commit", &old_entry.commit);
    printer.key_value("New commit", &new_commit);
    printer.key_value("New integrity", &new_integrity);

    // Check for signature on new ref
    enforce_signature_policy(
        cli,
        printer,
        Some(&new_ref),
        name,
        allow_unsigned,
        &cache_base,
        &old_git_src.repo_url,
    )?;

    // Confirm
    printer.newline();
    if !yes && !printer.prompt_confirm("Update this module?")? {
        printer.info("Cancelled");
        return Ok(());
    }

    // Update lockfile entry
    lockfile.modules[entry_idx] = cfgd_core::config::ModuleLockEntry {
        name: name.to_string(),
        url: old_entry.url.clone(),
        pinned_ref: new_ref,
        commit: new_commit,
        integrity: new_integrity,
        subdir: old_entry.subdir,
    };
    modules::save_lockfile(&config_dir, &lockfile)?;

    printer.success(&format!("Updated module '{}' in modules.lock", name));
    printer.info(MSG_RUN_APPLY);

    Ok(())
}

/// Print the module-review summary shown before the user confirms an
/// `add` or `upgrade`: dependencies, packages, files, post-apply script
/// warnings, then commit + integrity. Split out so the side-effect-free
/// output shape is testable against a captured Printer buffer without
/// running the full cmd_module_add_remote orchestration.
pub(super) fn print_module_review_summary(
    printer: &Printer,
    module_name: &str,
    module: &modules::LoadedModule,
    commit: &str,
    integrity: &str,
) {
    printer.newline();
    printer.subheader(&format!("Module: {}", module_name));

    if !module.spec.depends.is_empty() {
        printer.key_value("Dependencies", &module.spec.depends.join(", "));
    }

    if !module.spec.packages.is_empty() {
        printer.newline();
        printer.info(&format!("Packages ({}):", module.spec.packages.len()));
        for pkg in &module.spec.packages {
            let ver = pkg
                .min_version
                .as_ref()
                .map(|v| format!(" (min: {})", v))
                .unwrap_or_default();
            printer.info(&format!("  + {}{}", pkg.name, ver));
        }
    }

    if !module.spec.files.is_empty() {
        printer.newline();
        printer.info(&format!("Files ({}):", module.spec.files.len()));
        for file in &module.spec.files {
            printer.info(&format!("  {} -> {}", file.source, file.target));
        }
    }

    if let Some(ref scripts) = module.spec.scripts
        && !scripts.post_apply.is_empty()
    {
        printer.newline();
        printer.warning(&format!(
            "Post-apply scripts ({}) — these will execute on your machine:",
            scripts.post_apply.len()
        ));
        for script in &scripts.post_apply {
            printer.warning(&format!("  $ {}", script));
        }
    }

    printer.newline();
    printer.key_value("Commit", commit);
    printer.key_value("Integrity", integrity);
}

/// Filter a registry-module list by case-insensitive substring match on
/// the module name, and project each hit into the user-facing
/// `ModuleSearchResult` shape. Pure helper — split out so the
/// filter+projection logic is unit-testable against synthetic
/// `RegistryModule` slices without standing up a git registry.
pub(super) fn filter_and_build_search_results(
    registry_modules: &[modules::RegistryModule],
    query: &str,
) -> Vec<super::ModuleSearchResult> {
    let query_lower = query.to_lowercase();
    registry_modules
        .iter()
        .filter(|m| m.name.to_lowercase().contains(&query_lower))
        .map(|m| {
            let latest = m.tags.last().cloned();
            let desc = if m.description.is_empty() {
                None
            } else {
                Some(m.description.clone())
            };
            super::ModuleSearchResult {
                name: format!("{}/{}", m.registry, m.name),
                registry: m.registry.clone(),
                description: desc,
                version: latest,
            }
        })
        .collect()
}

pub(crate) fn cmd_module_search(cli: &Cli, printer: &Printer, query: &str) -> anyhow::Result<()> {
    let cache_base = modules::default_module_cache_dir()?;

    if !cli.config.exists() {
        anyhow::bail!("No config found — add module registries to cfgd.yaml first");
    }

    let cfg = config::load_config(&cli.config)?;
    let registries = cfg
        .spec
        .modules
        .as_ref()
        .map(|m| &m.registries[..])
        .unwrap_or(&[]);
    if registries.is_empty() {
        if printer.is_structured() {
            printer.write_structured(&Vec::<super::ModuleSearchResult>::new());
            return Ok(());
        }
        printer.header(&format!("Search Modules: {}", query));
        printer.info(NO_REGISTRIES_MSG);
        printer.info("Add a registry: cfgd module registry add <git-url>");
        return Ok(());
    }

    let mut all_results: Vec<super::ModuleSearchResult> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    for source in registries {
        match modules::fetch_registry_modules(source, &cache_base, printer) {
            Ok(registry_modules) => {
                all_results.extend(filter_and_build_search_results(&registry_modules, query));
            }
            Err(e) => {
                errors.push(format!("{}: {}", source.name, e));
            }
        }
    }

    if printer.write_structured(&all_results) {
        return Ok(());
    }

    printer.header(&format!("Search Modules: {}", query));

    for source in registries {
        printer.info(&format!("Searching {} ({})", source.name, source.url));
    }

    for err in &errors {
        printer.warning(&format!("  Failed to fetch source: {}", err));
    }

    if all_results.is_empty() {
        printer.newline();
        printer.info("No modules found matching your query");
    } else if printer.is_wide() {
        let rows: Vec<Vec<String>> = all_results
            .iter()
            .map(|m| {
                vec![
                    m.name.clone(),
                    m.registry.clone(),
                    m.description.clone().unwrap_or_default(),
                    m.version.clone().unwrap_or_else(|| "-".into()),
                ]
            })
            .collect();
        printer.table(&["Module", "Registry", "Description", "Latest"], &rows);
    } else {
        let rows: Vec<Vec<String>> = all_results
            .iter()
            .map(|m| {
                vec![
                    m.name.clone(),
                    m.description.clone().unwrap_or_default(),
                    m.version.clone().unwrap_or_else(|| "-".into()),
                ]
            })
            .collect();
        printer.table(&["Module", "Description", "Latest"], &rows);
    }

    Ok(())
}
pub(crate) fn cmd_module_registry_add(
    cli: &Cli,
    printer: &Printer,
    url: &str,
    name: Option<&str>,
) -> anyhow::Result<()> {
    printer.header("Add Module Registry");

    let registry_name = match name {
        Some(n) => n.to_string(),
        None => modules::extract_registry_name(url).ok_or_else(|| {
            anyhow::anyhow!("Cannot extract registry name from URL — use --name to specify one")
        })?,
    };

    if !cli.config.exists() {
        anyhow::bail!("{}", MSG_NO_CONFIG);
    }

    let new_entry = serde_yaml::to_value(cfgd_core::config::ModuleRegistryEntry {
        name: registry_name.clone(),
        url: url.to_string(),
    })?;

    // `already_present` short-circuits the "added" success message after the
    // helper's write (still a harmless idempotent rewrite).
    let mut already_present = false;
    super::mutate_config_yaml(&cli.config, true, |doc| {
        let spec = doc
            .get_mut("spec")
            .ok_or_else(|| anyhow::anyhow!("config has no spec"))?;
        if spec.get("modules").is_none() {
            spec["modules"] = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        }
        let modules = spec
            .get_mut("modules")
            .ok_or_else(|| anyhow::anyhow!("failed to create modules section"))?;
        let registries = modules
            .get_mut("registries")
            .and_then(|v| v.as_sequence_mut());

        if let Some(registries) = registries {
            if registries
                .iter()
                .any(|s| s.get("name").and_then(|v| v.as_str()) == Some(&registry_name))
            {
                already_present = true;
                return Ok(());
            }
            registries.push(new_entry.clone());
        } else {
            modules["registries"] = serde_yaml::Value::Sequence(vec![new_entry.clone()]);
        }
        Ok(())
    })?;

    if already_present {
        printer.info(&format!("Registry '{}' already configured", registry_name));
        return Ok(());
    }

    printer.success(&format!(
        "Added module registry '{}' ({})",
        registry_name, url
    ));
    printer.info("Search for modules: cfgd module search <query>");

    Ok(())
}

pub(crate) fn cmd_module_registry_remove(
    cli: &Cli,
    printer: &Printer,
    name: &str,
) -> anyhow::Result<()> {
    printer.header("Remove Module Registry");

    if !cli.config.exists() {
        anyhow::bail!("{}", MSG_NO_CONFIG);
    }

    let mut outcome = RegistryRemoveOutcome::NoRegistries;
    super::mutate_config_yaml(&cli.config, true, |doc| {
        let registries = doc
            .get_mut("spec")
            .and_then(|s| s.get_mut("modules"))
            .and_then(|m| m.get_mut("registries"))
            .and_then(|v| v.as_sequence_mut());
        match registries {
            None => outcome = RegistryRemoveOutcome::NoRegistries,
            Some(registries) => {
                let before = registries.len();
                registries.retain(|s| s.get("name").and_then(|v| v.as_str()) != Some(name));
                outcome = if registries.len() < before {
                    RegistryRemoveOutcome::Removed
                } else {
                    RegistryRemoveOutcome::NotFound
                };
            }
        }
        Ok(())
    })?;

    match outcome {
        RegistryRemoveOutcome::Removed => {
            printer.success(&format!("Removed module registry '{}'", name));
            // Warn about profile references that now point to a removed registry
            let config_dir = cli.config.parent().unwrap_or_else(|| Path::new("."));
            let profiles_dir = config_dir.join("profiles");
            let old_prefix = format!("{}/", name);
            cfgd_core::config::for_each_yaml_file(&profiles_dir, |path| {
                if let Ok(contents) = std::fs::read_to_string(path)
                    && contents.contains(&old_prefix)
                {
                    printer.warning(&format!(
                        "Profile '{}' still references '{}/...' — those modules will fail to resolve",
                        path.file_name().unwrap_or_default().to_string_lossy(),
                        name
                    ));
                }
                Ok(())
            })?;
        }
        RegistryRemoveOutcome::NotFound => {
            printer.info(&format!("Registry '{}' not found", name));
        }
        RegistryRemoveOutcome::NoRegistries => {
            printer.info(NO_REGISTRIES_MSG);
        }
    }

    Ok(())
}

enum RegistryRemoveOutcome {
    Removed,
    NotFound,
    NoRegistries,
}

pub(crate) fn cmd_module_registry_rename(
    cli: &Cli,
    printer: &Printer,
    name: &str,
    new_name: &str,
) -> anyhow::Result<()> {
    if !cli.config.exists() {
        anyhow::bail!("{}", MSG_NO_CONFIG);
    }

    let cfg = config::load_config(&cli.config)?;
    let registries = cfg
        .spec
        .modules
        .as_ref()
        .map(|m| &m.registries[..])
        .unwrap_or(&[]);

    if !registries.iter().any(|s| s.name == name) {
        anyhow::bail!("Registry '{}' not found", name);
    }
    if registries.iter().any(|s| s.name == new_name) {
        anyhow::bail!("A registry named '{}' already exists", new_name);
    }

    // Update registry name in cfgd.yaml via the shared mutate-write helper.
    super::mutate_config_yaml(&cli.config, true, |doc| {
        if let Some(registries) = doc
            .get_mut("spec")
            .and_then(|s| s.get_mut("modules"))
            .and_then(|m| m.get_mut("registries"))
            .and_then(|v| v.as_sequence_mut())
        {
            for entry in registries.iter_mut() {
                if entry.get("name").and_then(|v| v.as_str()) == Some(name) {
                    entry["name"] = serde_yaml::Value::String(new_name.to_string());
                    break;
                }
            }
        }
        Ok(())
    })?;

    // Cascade to profiles: old_name/module → new_name/module
    let profiles_dir = cli
        .config
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("profiles");
    let old_prefix = format!("{}/", name);
    let new_prefix = format!("{}/", new_name);
    cfgd_core::config::for_each_yaml_file(&profiles_dir, |path| {
        let contents = std::fs::read_to_string(path)?;
        let mut profile: config::ProfileDocument = match serde_yaml::from_str(&contents) {
            Ok(p) => p,
            Err(_) => return Ok(()),
        };
        let mut changed = false;
        for module_ref in &mut profile.spec.modules {
            if module_ref.starts_with(&old_prefix) {
                *module_ref = format!("{}{}", new_prefix, &module_ref[old_prefix.len()..]);
                changed = true;
            }
        }
        if changed {
            let yaml = serde_yaml::to_string(&profile).map_err(std::io::Error::other)?;
            cfgd_core::atomic_write_str(path, &yaml)?;
        }
        Ok(())
    })?;

    printer.success(&format!("Renamed registry '{}' to '{}'", name, new_name));
    Ok(())
}

pub(crate) fn cmd_module_registry_list(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    if !cli.config.exists() {
        if printer.is_structured() {
            printer.write_structured(&Vec::<super::RegistryListEntry>::new());
            return Ok(());
        }
        printer.header("Module Registries");
        printer.info("No config found");
        return Ok(());
    }

    let cfg = config::load_config(&cli.config)?;
    let registries = cfg
        .spec
        .modules
        .as_ref()
        .map(|m| &m.registries[..])
        .unwrap_or(&[]);
    if registries.is_empty() {
        if printer.is_structured() {
            printer.write_structured(&Vec::<super::RegistryListEntry>::new());
            return Ok(());
        }
        printer.header("Module Registries");
        printer.info(NO_REGISTRIES_MSG);
        printer.info("Add one: cfgd module registry add <git-url>");
        return Ok(());
    }

    let entries: Vec<super::RegistryListEntry> = registries
        .iter()
        .map(|s| super::RegistryListEntry {
            name: s.name.clone(),
            url: s.url.clone(),
        })
        .collect();

    if printer.write_structured(&entries) {
        return Ok(());
    }

    printer.header("Module Registries");

    let rows: Vec<Vec<String>> = entries
        .iter()
        .map(|e| vec![e.name.clone(), e.url.clone()])
        .collect();

    printer.table(&["Name", "URL"], &rows);

    Ok(())
}

/// Construct the git source URL the registry resolver feeds into
/// `cmd_module_add_remote`. The format is
/// `<registry_base_url>//modules/<module>@<module>/<tag>` — `parse_git_source`
/// treats the segment after `//` as a subdirectory and the segment after `@`
/// as the git tag, so this assembles a single URL that pins
/// `modules/<module>` at the per-module tag `<module>/<tag>` (the prefix lets
/// a single registry repo carry independently versioned modules).
pub(crate) fn build_registry_module_url(base_url: &str, module: &str, tag: &str) -> String {
    let subdir = format!("modules/{}", module);
    format!("{}//{}@{}/{}", base_url, subdir, module, tag)
}

/// Compute the URL string stored in the lockfile entry for a remote module.
///
/// When `git_src.subdir` is `None`, the raw URL the user supplied is preserved
/// verbatim. When a subdir is present the //subdir suffix is stripped (subdir
/// rides in its own `ModuleLockEntry::subdir` field) but tag/ref are
/// re-attached in the canonical `repo@tag` / `repo?ref=branch` shapes — so
/// `cmd_module_upgrade` can later `parse_git_source(url)` and recover the
/// same source coordinates.
pub(super) fn compute_lock_url(raw_url: &str, git_src: &modules::GitSource) -> String {
    if git_src.subdir.is_some() {
        let mut clean = git_src.repo_url.clone();
        if let Some(ref t) = git_src.tag {
            clean = format!("{}@{}", clean, t);
        } else if let Some(ref r) = git_src.git_ref {
            clean = format!("{}?ref={}", clean, r);
        }
        clean
    } else {
        raw_url.to_string()
    }
}

/// Compute the `pinned_ref` field for a `ModuleLockEntry`.
///
/// Precedence: explicit tag > explicit branch ref > commit SHA fallback. The
/// commit SHA fallback is what `cfgd module apply` checks against the remote
/// when the user pinned `@HEAD` or omitted the ref — the lockfile always
/// stores *something* so the reconciler has a deterministic target.
pub(super) fn compute_pinned_ref(git_src: &modules::GitSource, commit: &str) -> String {
    git_src
        .tag
        .clone()
        .or_else(|| git_src.git_ref.clone())
        .unwrap_or_else(|| commit.to_string())
}

/// Append `module_ref` to a profile's `spec.modules` if not already listed.
///
/// Returns `true` if the document was mutated (so the caller should rewrite
/// it to disk), `false` if `module_ref` was already present (no I/O needed).
/// Idempotent — repeated calls with the same `module_ref` are no-ops after
/// the first.
pub(super) fn ensure_module_in_profile_doc(
    doc: &mut config::ProfileDocument,
    module_ref: &str,
) -> bool {
    if doc.spec.modules.iter().any(|m| m == module_ref) {
        return false;
    }
    doc.spec.modules.push(module_ref.to_string());
    true
}
