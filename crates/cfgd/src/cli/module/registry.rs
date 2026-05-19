use super::*;
use cfgd_core::output_v2::{Doc, Printer as PrinterV2, Role};

pub fn cmd_module_add_from_registry(
    cli: &Cli,
    v2_printer: &PrinterV2,
    reference: &str,
    yes: bool,
    allow_unsigned: bool,
) -> anyhow::Result<()> {
    let reg_ref = match modules::parse_registry_ref(reference) {
        Some(r) => r,
        None => {
            v2_printer.emit(cfgd_core::output_v2::error_doc(
                reference,
                "invalid_reference",
                format!(
                    "Invalid registry reference '{}' — expected registry/module[@tag]",
                    reference
                ),
                serde_json::json!({}),
            ));
            anyhow::bail!(
                "Invalid registry reference '{}' — expected registry/module[@tag]",
                reference
            );
        }
    };

    // Load config to find the registry
    if !cli.config.exists() {
        v2_printer.emit(cfgd_core::output_v2::error_doc(
            reference,
            "no_config",
            MSG_NO_CONFIG.to_string(),
            serde_json::json!({}),
        ));
        anyhow::bail!("{}", MSG_NO_CONFIG);
    }
    let cfg = config::load_config(&cli.config)?;

    let registries = cfg
        .spec
        .modules
        .as_ref()
        .map(|m| &m.registries[..])
        .unwrap_or(&[]);
    let registry_entry = match registries.iter().find(|s| s.name == reg_ref.registry) {
        Some(r) => r,
        None => {
            v2_printer.emit(cfgd_core::output_v2::error_doc(
                &reg_ref.registry,
                "registry_not_found",
                format!(
                    "Registry '{}' not configured — run 'cfgd module registry add <url>' first",
                    reg_ref.registry
                ),
                serde_json::json!({}),
            ));
            anyhow::bail!(
                "Registry '{}' not configured — run 'cfgd module registry add <url>' first",
                reg_ref.registry
            );
        }
    };

    // Resolve the tag — use explicit tag or find latest
    let cache_base = modules::default_module_cache_dir()?;
    let tag = match reg_ref.tag {
        Some(t) => t,
        None => {
            v2_printer.status_simple(
                Role::Info,
                format!(
                    "No tag specified — looking up latest for '{}'",
                    reg_ref.module
                ),
            );
            // Fetch the registry repo so we can read tags. The lib call
            // takes &Printer; the null sink suppresses its v1 progress so
            // this command's v2 status surface above owns the user-facing
            // line.
            let printer = null_lib_printer();
            modules::fetch_registry_modules(registry_entry, &cache_base, &printer)?;
            match modules::latest_module_version(registry_entry, &reg_ref.module, &cache_base)? {
                Some(t) => t,
                None => {
                    v2_printer.emit(cfgd_core::output_v2::error_doc(
                        &reg_ref.module,
                        "version_not_found",
                        format!(
                            "No tags found for module '{}' in registry '{}'",
                            reg_ref.module, reg_ref.registry
                        ),
                        serde_json::json!({ "registry": &reg_ref.registry }),
                    ));
                    anyhow::bail!(
                        "No tags found for module '{}' in registry '{}'",
                        reg_ref.module,
                        reg_ref.registry
                    );
                }
            }
        }
    };

    // Build the full git URL with subdir and delegate to cmd_module_add_remote
    let full_url = build_registry_module_url(&registry_entry.url, &reg_ref.module, &tag);
    v2_printer.status_simple(
        Role::Info,
        format!(
            "Resolved: {}/{} -> {}",
            reg_ref.registry, reg_ref.module, full_url
        ),
    );

    cmd_module_add_remote(
        cli,
        v2_printer,
        &full_url,
        Some(&reg_ref.registry),
        yes,
        allow_unsigned,
    )
}

pub fn cmd_module_add_remote(
    cli: &Cli,
    v2_printer: &PrinterV2,
    url: &str,
    source_name: Option<&str>,
    yes: bool,
    allow_unsigned: bool,
) -> anyhow::Result<()> {
    v2_printer.heading("Add Remote Module");

    let config_dir = config_dir(cli);
    let cache_base = modules::default_module_cache_dir()?;

    // Streaming: clone-fetch spinner. The lib call takes &Printer; the
    // null sink suppresses its v1 progress so the v2 spinner owns the
    // user-facing surface.
    let sp = v2_printer.spinner(format!("Fetching {}", url));
    let printer = null_lib_printer();
    let fetched = match modules::fetch_remote_module(url, &cache_base, &printer) {
        Ok(f) => {
            sp.finish_ok(format!("Fetched {}", url));
            f
        }
        Err(e) => {
            sp.finish_fail(format!("Failed to fetch {}", url))
                .detail(e.to_string());
            v2_printer.emit(cfgd_core::output_v2::error_doc(
                url,
                "clone_failed",
                e.to_string(),
                serde_json::json!({}),
            ));
            return Err(anyhow::anyhow!(e));
        }
    };
    let module_name = fetched.module.name.clone();
    let module = fetched.module;
    let commit = fetched.commit;
    let integrity = fetched.integrity;

    // Check if already in lockfile
    let mut lockfile = modules::load_lockfile(&config_dir)?;
    if lockfile.modules.iter().any(|e| e.name == module_name) {
        v2_printer.emit(
            Doc::new()
                .status(
                    Role::Info,
                    format!(
                        "Module '{}' is already in the lockfile — use 'cfgd module update {}' to change versions",
                        module_name, module_name
                    ),
                )
                .with_data(serde_json::json!({
                    "name": module_name,
                    "alreadyLocked": true,
                })),
        );
        return Ok(());
    }

    // Check if a local module with the same name exists
    let local_modules = modules::load_modules(&config_dir)?;
    if local_modules.contains_key(&module_name) {
        v2_printer.emit(cfgd_core::output_v2::error_doc(
            &module_name,
            "already_exists",
            format!(
                "Local module '{}' already exists in {}/modules/ — local modules take precedence over remote",
                module_name,
                config_dir.display()
            ),
            serde_json::json!({ "configDir": config_dir.display().to_string() }),
        ));
        anyhow::bail!(
            "Local module '{}' already exists in {}/modules/ — local modules take precedence over remote",
            module_name,
            config_dir.display()
        );
    }

    print_module_review_summary(v2_printer, &module_name, &module, &commit, &integrity);

    // Check for GPG/SSH signature on the tag
    let git_src = modules::parse_git_source(url)?;
    super::enforce_signature_policy(
        cli,
        v2_printer,
        git_src.tag.as_deref(),
        &module_name,
        allow_unsigned,
        &cache_base,
        &git_src.repo_url,
    )?;

    // Confirm
    if !yes && !v2_printer.prompt_confirm("Add this remote module?")? {
        v2_printer.emit(
            Doc::new()
                .status(Role::Info, "Cancelled")
                .with_data(serde_json::json!({
                    "name": module_name,
                    "url": url,
                    "cancelled": true,
                })),
        );
        return Ok(());
    }

    // Build clean lockfile URL: repo@tag (no //subdir — subdir is a separate field)
    let lock_url = compute_lock_url(url, &git_src);

    let pinned_ref = compute_pinned_ref(&git_src, &commit);

    let lock_entry = cfgd_core::config::ModuleLockEntry {
        name: module_name.clone(),
        url: lock_url,
        pinned_ref: pinned_ref.clone(),
        commit: commit.clone(),
        integrity: integrity.clone(),
        subdir: git_src.subdir,
    };
    lockfile.modules.push(lock_entry);
    modules::save_lockfile(&config_dir, &lockfile)?;

    // Add to profile if config exists — use registry/module format for registry-backed modules
    let profile_module_ref = match source_name {
        Some(src) => format!("{}/{}", src, module_name),
        None => module_name.clone(),
    };
    let mut added_to_profile: Option<String> = None;
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
                v2_printer.status_simple(
                    Role::Ok,
                    format!(
                        "Added module '{}' to profile '{}'",
                        profile_module_ref, profile_name
                    ),
                );
                added_to_profile = Some(profile_name.to_string());
            }
        }
    }

    v2_printer.emit(
        Doc::new()
            .status(
                Role::Ok,
                format!("Locked remote module '{}' in modules.lock", module_name),
            )
            .hint(MSG_RUN_APPLY)
            .with_data(serde_json::json!({
                "name": module_name,
                "url": url,
                "commit": commit,
                "integrity": integrity,
                "pinnedRef": pinned_ref,
                "addedToProfile": added_to_profile,
            })),
    );

    Ok(())
}

pub fn cmd_module_upgrade(
    cli: &Cli,
    v2_printer: &PrinterV2,
    name: &str,
    new_ref: Option<&str>,
    yes: bool,
    allow_unsigned: bool,
) -> anyhow::Result<()> {
    v2_printer.heading(format!("Update Module: {}", name));

    let config_dir = config_dir(cli);
    let cache_base = modules::default_module_cache_dir()?;
    let printer = null_lib_printer();

    // Find the lockfile entry
    let mut lockfile = modules::load_lockfile(&config_dir)?;
    let entry_idx = lockfile.modules.iter().position(|e| e.name == name);

    let entry_idx = match entry_idx {
        Some(idx) => idx,
        None => {
            let local_modules = modules::load_modules(&config_dir)?;
            if local_modules.contains_key(name) {
                v2_printer.emit(cfgd_core::output_v2::error_doc(
                    name,
                    "local_module",
                    format!(
                        "Module '{}' is a local module — edit it directly in {}/modules/{}/",
                        name,
                        config_dir.display(),
                        name
                    ),
                    serde_json::json!({}),
                ));
                anyhow::bail!(
                    "Module '{}' is a local module — edit it directly in {}/modules/{}/",
                    name,
                    config_dir.display(),
                    name
                );
            } else {
                v2_printer.emit(cfgd_core::output_v2::error_doc(
                    name,
                    "not_found",
                    format!("Module '{}' not found", name),
                    serde_json::json!({}),
                ));
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
    let old_local_path = modules::fetch_git_source(&old_pinned_src, &cache_base, name, &printer)?;
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
            modules::fetch_git_source(&git_src, &cache_base, name, &printer)?;
            let repo_dir = modules::git_cache_dir(&cache_base, &old_git_src.repo_url);
            let head = modules::get_head_commit_sha(&repo_dir)?;
            v2_printer.status_simple(Role::Info, format!("Latest commit: {}", head));
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
    let new_local_path = modules::fetch_git_source(&new_src, &cache_base, name, &printer)?;
    let new_module = modules::load_module(&new_local_path)?;
    let repo_dir = modules::git_cache_dir(&cache_base, &old_git_src.repo_url);
    let new_commit = modules::get_head_commit_sha(&repo_dir)?;
    let new_integrity = modules::hash_module_contents(&new_local_path)?;

    if new_commit == old_entry.commit {
        v2_printer.emit(
            Doc::new()
                .status(Role::Info, "Module is already at this version")
                .with_data(serde_json::json!({
                    "name": name,
                    "commit": new_commit,
                    "noChange": true,
                })),
        );
        return Ok(());
    }

    // Show diff
    let changes = modules::diff_module_specs(&old_module, &new_module);
    {
        let changes_sec = v2_printer.section("Changes");
        for change in &changes {
            changes_sec.bullet(change);
        }
    }

    v2_printer.kv_block([
        ("Old commit", old_entry.commit.as_str()),
        ("New commit", new_commit.as_str()),
        ("New integrity", new_integrity.as_str()),
    ]);

    // Check for signature on new ref
    super::enforce_signature_policy(
        cli,
        v2_printer,
        Some(&new_ref),
        name,
        allow_unsigned,
        &cache_base,
        &old_git_src.repo_url,
    )?;

    // Confirm
    if !yes && !v2_printer.prompt_confirm("Update this module?")? {
        v2_printer.emit(
            Doc::new()
                .status(Role::Info, "Cancelled")
                .with_data(serde_json::json!({
                    "name": name,
                    "cancelled": true,
                })),
        );
        return Ok(());
    }

    // Update lockfile entry
    lockfile.modules[entry_idx] = cfgd_core::config::ModuleLockEntry {
        name: name.to_string(),
        url: old_entry.url.clone(),
        pinned_ref: new_ref.clone(),
        commit: new_commit.clone(),
        integrity: new_integrity.clone(),
        subdir: old_entry.subdir,
    };
    modules::save_lockfile(&config_dir, &lockfile)?;

    v2_printer.emit(
        Doc::new()
            .status(
                Role::Ok,
                format!("Updated module '{}' in modules.lock", name),
            )
            .hint(MSG_RUN_APPLY)
            .with_data(serde_json::json!({
                "name": name,
                "oldCommit": old_entry.commit,
                "newCommit": new_commit,
                "pinnedRef": new_ref,
                "integrity": new_integrity,
            })),
    );

    Ok(())
}

/// Print the module-review summary shown before the user confirms an
/// `add` or `upgrade`: dependencies, packages, files, post-apply script
/// warnings, then commit + integrity. Split out so the side-effect-free
/// output shape is testable against a captured Printer buffer without
/// running the full cmd_module_add_remote orchestration.
pub(super) fn print_module_review_summary(
    v2_printer: &PrinterV2,
    module_name: &str,
    module: &modules::LoadedModule,
    commit: &str,
    integrity: &str,
) {
    let mod_sec = v2_printer.section(format!("Module: {}", module_name));

    if !module.spec.depends.is_empty() {
        mod_sec.kv("Dependencies", module.spec.depends.join(", "));
    }

    if !module.spec.packages.is_empty() {
        let pkgs_sec = mod_sec.section(format!("Packages ({})", module.spec.packages.len()));
        for pkg in &module.spec.packages {
            let ver = pkg
                .min_version
                .as_ref()
                .map(|v| format!(" (min: {})", v))
                .unwrap_or_default();
            pkgs_sec.bullet(format!("{}{}", pkg.name, ver));
        }
    }

    if !module.spec.files.is_empty() {
        let files_sec = mod_sec.section(format!("Files ({})", module.spec.files.len()));
        for file in &module.spec.files {
            files_sec.bullet(format!("{} -> {}", file.source, file.target));
        }
    }

    if let Some(ref scripts) = module.spec.scripts
        && !scripts.post_apply.is_empty()
    {
        mod_sec.status_simple(
            Role::Warn,
            format!(
                "Post-apply scripts ({}) — these will execute on your machine:",
                scripts.post_apply.len()
            ),
        );
        let scripts_sec = mod_sec.section("Post-apply");
        for script in &scripts.post_apply {
            scripts_sec.bullet(format!("$ {}", script));
        }
    }

    mod_sec.kv("Commit", commit);
    mod_sec.kv("Integrity", integrity);
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

pub fn cmd_module_search(cli: &Cli, v2_printer: &PrinterV2, query: &str) -> anyhow::Result<()> {
    let cache_base = modules::default_module_cache_dir()?;
    let printer = null_lib_printer();

    if !cli.config.exists() {
        v2_printer.emit(cfgd_core::output_v2::error_doc(
            query,
            "no_config",
            "No config found — add module registries to cfgd.yaml first".to_string(),
            serde_json::json!({}),
        ));
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
        v2_printer.emit(
            Doc::new()
                .heading(format!("Search Modules: {}", query))
                .status(Role::Info, NO_REGISTRIES_MSG)
                .hint("Add a registry: cfgd module registry add <git-url>")
                .with_data(serde_json::json!([])),
        );
        return Ok(());
    }

    let mut all_results: Vec<super::ModuleSearchResult> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    for source in registries {
        let sp = v2_printer.spinner(format!("Searching {} ({})", source.name, source.url));
        match modules::fetch_registry_modules(source, &cache_base, &printer) {
            Ok(registry_modules) => {
                sp.finish_ok(format!("Searched {}", source.name));
                all_results.extend(filter_and_build_search_results(&registry_modules, query));
            }
            Err(e) => {
                sp.finish_fail(format!("Failed to fetch source: {}", source.name))
                    .detail(e.to_string());
                errors.push(format!("{}: {}", source.name, e));
            }
        }
    }

    let mut doc = Doc::new().heading(format!("Search Modules: {}", query));

    if !errors.is_empty() {
        let mut errs_doc = doc;
        errs_doc = errs_doc.section_if_nonempty("Errors", &errors, |sec, errs| {
            errs.iter()
                .fold(sec, |s, e| s.status(Role::Warn, e.clone()))
        });
        doc = errs_doc;
    }

    if all_results.is_empty() {
        doc = doc.status(Role::Info, "No modules found matching your query");
    } else if v2_printer.is_wide() {
        let mut t = cfgd_core::output_v2::renderer::Table::new([
            "Module",
            "Registry",
            "Description",
            "Latest",
        ]);
        for m in &all_results {
            t = t.row([
                m.name.clone(),
                m.registry.clone(),
                m.description.clone().unwrap_or_default(),
                m.version.clone().unwrap_or_else(|| "-".into()),
            ]);
        }
        doc = doc.table(t);
    } else {
        let mut t = cfgd_core::output_v2::renderer::Table::new(["Module", "Description", "Latest"]);
        for m in &all_results {
            t = t.row([
                m.name.clone(),
                m.description.clone().unwrap_or_default(),
                m.version.clone().unwrap_or_else(|| "-".into()),
            ]);
        }
        doc = doc.table(t);
    }

    v2_printer.emit(doc.with_data(&all_results));
    Ok(())
}

pub fn cmd_module_registry_add(
    cli: &Cli,
    v2_printer: &PrinterV2,
    url: &str,
    name: Option<&str>,
) -> anyhow::Result<()> {
    v2_printer.heading("Add Module Registry");

    let registry_name = match name {
        Some(n) => n.to_string(),
        None => match modules::extract_registry_name(url) {
            Some(n) => n,
            None => {
                v2_printer.emit(cfgd_core::output_v2::error_doc(
                    url,
                    "invalid_url",
                    "Cannot extract registry name from URL — use --name to specify one".to_string(),
                    serde_json::json!({}),
                ));
                anyhow::bail!("Cannot extract registry name from URL — use --name to specify one");
            }
        },
    };

    if !cli.config.exists() {
        v2_printer.emit(cfgd_core::output_v2::error_doc(
            &registry_name,
            "no_config",
            MSG_NO_CONFIG.to_string(),
            serde_json::json!({}),
        ));
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
        v2_printer.emit(
            Doc::new()
                .status(
                    Role::Info,
                    format!("Registry '{}' already configured", registry_name),
                )
                .with_data(serde_json::json!({
                    "name": registry_name,
                    "url": url,
                    "alreadyConfigured": true,
                })),
        );
        return Ok(());
    }

    v2_printer.emit(
        Doc::new()
            .status(
                Role::Ok,
                format!("Added module registry '{}' ({})", registry_name, url),
            )
            .hint("Search for modules: cfgd module search <query>")
            .with_data(serde_json::json!({
                "name": registry_name,
                "url": url,
                "alreadyConfigured": false,
            })),
    );

    Ok(())
}

pub fn cmd_module_registry_remove(
    cli: &Cli,
    v2_printer: &PrinterV2,
    name: &str,
) -> anyhow::Result<()> {
    v2_printer.heading("Remove Module Registry");

    if !cli.config.exists() {
        v2_printer.emit(cfgd_core::output_v2::error_doc(
            name,
            "no_config",
            MSG_NO_CONFIG.to_string(),
            serde_json::json!({}),
        ));
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
            // Warn about profile references that now point to a removed registry
            let config_dir = cli.config.parent().unwrap_or_else(|| Path::new("."));
            let profiles_dir = config_dir.join("profiles");
            let old_prefix = format!("{}/", name);
            let mut affected_profiles: Vec<String> = Vec::new();
            cfgd_core::config::for_each_yaml_file(&profiles_dir, |path| {
                if let Ok(contents) = std::fs::read_to_string(path)
                    && contents.contains(&old_prefix)
                {
                    let profile_name = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    affected_profiles.push(profile_name);
                }
                Ok(())
            })?;

            if !affected_profiles.is_empty() {
                let warn_sec = v2_printer.section("Profile references");
                for profile_name in &affected_profiles {
                    warn_sec.status_simple(
                        Role::Warn,
                        format!(
                            "Profile '{}' still references '{}/...' — those modules will fail to resolve",
                            profile_name, name
                        ),
                    );
                }
            }

            v2_printer.emit(
                Doc::new()
                    .status(Role::Ok, format!("Removed module registry '{}'", name))
                    .with_data(serde_json::json!({
                        "name": name,
                        "outcome": "removed",
                        "affectedProfiles": affected_profiles,
                    })),
            );
        }
        RegistryRemoveOutcome::NotFound => {
            v2_printer.emit(
                Doc::new()
                    .status(Role::Info, format!("Registry '{}' not found", name))
                    .with_data(serde_json::json!({
                        "name": name,
                        "outcome": "not_found",
                    })),
            );
        }
        RegistryRemoveOutcome::NoRegistries => {
            v2_printer.emit(Doc::new().status(Role::Info, NO_REGISTRIES_MSG).with_data(
                serde_json::json!({
                    "name": name,
                    "outcome": "no_registries",
                }),
            ));
        }
    }

    Ok(())
}

enum RegistryRemoveOutcome {
    Removed,
    NotFound,
    NoRegistries,
}

pub fn cmd_module_registry_rename(
    cli: &Cli,
    v2_printer: &PrinterV2,
    name: &str,
    new_name: &str,
) -> anyhow::Result<()> {
    if !cli.config.exists() {
        v2_printer.emit(cfgd_core::output_v2::error_doc(
            name,
            "no_config",
            MSG_NO_CONFIG.to_string(),
            serde_json::json!({}),
        ));
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
        v2_printer.emit(cfgd_core::output_v2::error_doc(
            name,
            "registry_not_found",
            format!("Registry '{}' not found", name),
            serde_json::json!({}),
        ));
        anyhow::bail!("Registry '{}' not found", name);
    }
    if registries.iter().any(|s| s.name == new_name) {
        v2_printer.emit(cfgd_core::output_v2::error_doc(
            new_name,
            "already_exists",
            format!("A registry named '{}' already exists", new_name),
            serde_json::json!({}),
        ));
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

    v2_printer.emit(
        Doc::new()
            .status(
                Role::Ok,
                format!("Renamed registry '{}' to '{}'", name, new_name),
            )
            .with_data(serde_json::json!({
                "from": name,
                "to": new_name,
            })),
    );
    Ok(())
}

pub fn cmd_module_registry_list(cli: &Cli, v2_printer: &PrinterV2) -> anyhow::Result<()> {
    if !cli.config.exists() {
        v2_printer.emit(
            Doc::new()
                .heading("Module Registries")
                .status(Role::Info, "No config found")
                .with_data(serde_json::json!([])),
        );
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
        v2_printer.emit(
            Doc::new()
                .heading("Module Registries")
                .status(Role::Info, NO_REGISTRIES_MSG)
                .hint("Add one: cfgd module registry add <git-url>")
                .with_data(serde_json::json!([])),
        );
        return Ok(());
    }

    let entries: Vec<super::RegistryListEntry> = registries
        .iter()
        .map(|s| super::RegistryListEntry {
            name: s.name.clone(),
            url: s.url.clone(),
        })
        .collect();

    let mut t = cfgd_core::output_v2::renderer::Table::new(["Name", "URL"]);
    for e in &entries {
        t = t.row([e.name.clone(), e.url.clone()]);
    }

    v2_printer.emit(
        Doc::new()
            .heading("Module Registries")
            .table(t)
            .with_data(&entries),
    );

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

/// Construct a Quiet Printer for outbound `cfgd_core::modules::*` library
/// calls. The lib owns its own progress surface; this CLI module already
/// drives the user-facing v2 spinner / status above the call, so the lib's
/// emissions are suppressed (inversion of control — the CLI owns the
/// user-facing surface, the lib gets a non-emitting sink).
fn null_lib_printer() -> cfgd_core::output_v2::Printer {
    cfgd_core::output_v2::Printer::new(cfgd_core::output_v2::Verbosity::Quiet)
}
