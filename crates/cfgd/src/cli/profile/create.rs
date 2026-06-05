use super::*;
use cfgd_core::PathDisplayExt;
use cfgd_core::output::{Doc, Printer, Role};

pub fn cmd_profile_create(
    cli: &Cli,
    printer: &Printer,
    args: &ProfileCreateArgs,
) -> anyhow::Result<()> {
    let name = &args.name;
    let inherits = &args.inherits;
    let module_list = &args.modules;
    let pkg_list = &args.packages;
    let var_list = &args.env;
    let alias_list = &args.aliases;
    let sys_list = &args.system;
    let files = &args.files;
    let secret_list = &args.secrets;
    let pre_apply = &args.pre_apply;
    let post_apply = &args.post_apply;
    let pre_reconcile = &args.pre_reconcile;
    let post_reconcile = &args.post_reconcile;
    let on_change = &args.on_change;
    let on_drift = &args.on_drift;
    validate_resource_name(name, "Profile")?;
    printer.heading(format!("Create Profile: {}", name));

    let config_dir = config_dir(cli);
    let pdir = config_dir.join("profiles");
    std::fs::create_dir_all(&pdir)?;

    let profile_path = pdir.join(format!("{}.yaml", name));
    if profile_path.exists() {
        return Err(crate::cli::cli_error(
            name,
            "already_exists",
            format!(
                "Profile '{}' already exists at {}",
                name,
                profile_path.posix()
            ),
            serde_json::json!({ "path": cfgd_core::to_posix_string(&profile_path) }),
        ));
    }

    // Verify inherited profiles exist
    for parent in inherits {
        let parent_path = pdir.join(format!("{}.yaml", parent));
        if !parent_path.exists() {
            return Err(crate::cli::cli_error(
                name,
                "parent_not_found",
                format!("Parent profile '{}' not found", parent),
                serde_json::json!({ "parent": parent }),
            ));
        }
    }

    // Interactive mode if no flags
    let is_interactive = inherits.is_empty()
        && module_list.is_empty()
        && pkg_list.is_empty()
        && var_list.is_empty()
        && alias_list.is_empty()
        && sys_list.is_empty()
        && files.is_empty()
        && secret_list.is_empty()
        && pre_apply.is_empty()
        && post_apply.is_empty()
        && pre_reconcile.is_empty()
        && post_reconcile.is_empty()
        && on_change.is_empty()
        && on_drift.is_empty();

    let (inh, mods, pkgs_parsed, vars, sys) = if is_interactive {
        let inh_str = printer.prompt_text("Inherit from (comma-separated, or empty)", "")?;
        let inh: Vec<String> = if inh_str.is_empty() {
            Vec::new()
        } else {
            inh_str.split(',').map(|s| s.trim().to_string()).collect()
        };
        for parent in &inh {
            let parent_path = pdir.join(format!("{}.yaml", parent));
            if !parent_path.exists() {
                return Err(crate::cli::cli_error(
                    name,
                    "parent_not_found",
                    format!("Parent profile '{}' not found", parent),
                    serde_json::json!({ "parent": parent }),
                ));
            }
        }

        let mods_str = printer.prompt_text("Modules (comma-separated, or empty)", "")?;
        let mods: Vec<String> = if mods_str.is_empty() {
            Vec::new()
        } else {
            mods_str.split(',').map(|s| s.trim().to_string()).collect()
        };

        (inh, mods, Vec::new(), Vec::new(), Vec::new())
    } else {
        let known = super::known_manager_names();
        let known_refs: Vec<&str> = known.iter().map(|s| s.as_str()).collect();
        let default_mgr = Platform::detect().native_manager().to_string();
        let pkgs = pkg_list
            .iter()
            .map(|s| {
                let (mgr, pkg) = super::parse_package_flag(s, &known_refs);
                (mgr.unwrap_or_else(|| default_mgr.clone()), pkg)
            })
            .collect::<Vec<_>>();
        let vars = var_list.to_vec();
        let sys = sys_list.to_vec();
        (inherits.to_vec(), module_list.to_vec(), pkgs, vars, sys)
    };

    // Warn about modules that don't exist locally (could be remote)
    let modules_dir = config_dir.join("modules");
    for m in &mods {
        if !modules_dir.join(m).join("module.yaml").exists() {
            printer.status_simple(
                Role::Warn,
                format!(
                    "Module '{}' not found locally — make sure it exists or is a remote module",
                    m
                ),
            );
        }
    }

    // Build packages spec
    let mut packages_spec = config::PackagesSpec::default();
    for (mgr, pkg) in &pkgs_parsed {
        packages::add_package(mgr, pkg, &mut packages_spec)?;
    }
    let has_packages = !pkgs_parsed.is_empty();

    // Build env
    let mut env_vars = Vec::new();
    for v in &vars {
        env_vars.push(cfgd_core::parse_env_var(v).map_err(|e| anyhow::anyhow!(e))?);
    }

    // Build aliases
    let mut shell_aliases = Vec::new();
    for a in alias_list {
        shell_aliases.push(cfgd_core::parse_alias(a).map_err(|e| anyhow::anyhow!(e))?);
    }

    // Build system settings
    let mut system = std::collections::HashMap::new();
    for s in &sys {
        let (key, value) = s.split_once('=').ok_or_else(|| {
            anyhow::anyhow!("Invalid system setting '{}' — expected key=value", s)
        })?;
        system.insert(
            key.to_string(),
            serde_yaml::Value::String(value.to_string()),
        );
    }

    // Copy files
    let files_dir = config_dir.join("profiles").join(name).join("files");
    let copied = copy_files_to_dir(files, &files_dir)?;
    let is_private = args.private;
    let file_entries: Vec<config::ManagedFileSpec> = copied
        .iter()
        .map(|(basename, deploy_target)| config::ManagedFileSpec {
            source: format!("profiles/{}/files/{}", name, basename),
            target: deploy_target.clone(),
            strategy: None,
            private: is_private,
            origin: None,
            encryption: None,
            permissions: None,
        })
        .collect();
    if is_private {
        for (basename, _) in &copied {
            add_to_gitignore(
                &config_dir,
                &format!("profiles/{}/files/{}", name, basename),
            )?;
        }
    }

    // Build secrets
    let secrets = secret_list
        .iter()
        .map(|s| parse_secret_spec(s))
        .collect::<anyhow::Result<Vec<_>>>()?;

    // Build scripts
    let has_scripts = !pre_apply.is_empty()
        || !post_apply.is_empty()
        || !pre_reconcile.is_empty()
        || !post_reconcile.is_empty()
        || !on_change.is_empty()
        || !on_drift.is_empty();
    let scripts = if !has_scripts {
        None
    } else {
        Some(config::ScriptSpec {
            pre_apply: pre_apply
                .iter()
                .map(|s| config::ScriptEntry::Simple(s.clone()))
                .collect(),
            post_apply: post_apply
                .iter()
                .map(|s| config::ScriptEntry::Simple(s.clone()))
                .collect(),
            pre_reconcile: pre_reconcile
                .iter()
                .map(|s| config::ScriptEntry::Simple(s.clone()))
                .collect(),
            post_reconcile: post_reconcile
                .iter()
                .map(|s| config::ScriptEntry::Simple(s.clone()))
                .collect(),
            on_change: on_change
                .iter()
                .map(|s| config::ScriptEntry::Simple(s.clone()))
                .collect(),
            on_drift: on_drift
                .iter()
                .map(|s| config::ScriptEntry::Simple(s.clone()))
                .collect(),
        })
    };

    let doc = config::ProfileDocument {
        api_version: cfgd_core::API_VERSION.to_string(),
        kind: "Profile".to_string(),
        metadata: config::ProfileMetadata {
            name: name.to_string(),
        },
        spec: config::ProfileSpec {
            inherits: inh,
            modules: mods,
            env: env_vars,
            env_scope: None,
            aliases: shell_aliases,
            packages: if has_packages {
                Some(packages_spec)
            } else {
                None
            },
            files: if file_entries.is_empty() {
                None
            } else {
                Some(config::FilesSpec {
                    managed: file_entries,
                    permissions: std::collections::HashMap::new(),
                })
            },
            system,
            secrets,
            scripts,
        },
    };

    let yaml = serde_yaml::to_string(&doc)?;
    cfgd_core::atomic_write_str(&profile_path, &yaml)?;

    let mut out = Doc::new().status(
        Role::Ok,
        format!("Created profile '{}' at {}", name, profile_path.posix()),
    );
    if !doc.spec.inherits.is_empty() {
        out = out.kv("Inherits", doc.spec.inherits.join(", "));
    }
    if !doc.spec.modules.is_empty() {
        out = out.kv("Modules", doc.spec.modules.join(", "));
    }
    out = out
        .hint(format!("Activate with: cfgd profile switch {}", name))
        .with_data(serde_json::json!({
            "name": name,
            "path": profile_path.display().to_string(),
            "inherits": doc.spec.inherits,
            "modules": doc.spec.modules,
        }));
    printer.emit(out);

    maybe_update_workflow(cli, printer)?;

    Ok(())
}
