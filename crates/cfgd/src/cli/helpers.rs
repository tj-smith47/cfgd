use super::*;
use cfgd_core::PathDisplayExt;
use cfgd_core::output::{Printer, Role};

pub(in crate::cli) fn load_config_and_profile(
    cli: &Cli,
) -> anyhow::Result<(CfgdConfig, String, ResolvedProfile)> {
    let cfg = config::load_config(&cli.config)?;
    let profile_name = match cli.profile.as_deref() {
        Some(p) => p.to_string(),
        None => cfg.active_profile()?.to_string(),
    };
    let resolved = config::resolve_profile(&profile_name, &profiles_dir(cli))?;
    Ok((cfg, profile_name, resolved))
}

/// Parse a `--package` flag value. If it contains `:` and the prefix is a known
/// package manager name, split into (Some(manager), package). Otherwise treat
/// the entire string as a bare package name.
pub(in crate::cli) fn parse_package_flag(
    s: &str,
    known_managers: &[&str],
) -> (Option<String>, String) {
    if let Some((prefix, suffix)) = s.split_once(':')
        && !prefix.is_empty()
        && !suffix.is_empty()
        && known_managers.contains(&prefix)
    {
        return (Some(prefix.to_string()), suffix.to_string());
    }
    (None, s.to_string())
}

/// Build an empty ResolvedProfile for module-only operations that don't need
/// a real profile (status --module, verify --module, apply --module without profile).
pub(in crate::cli) fn empty_resolved_profile(module_name: &str) -> ResolvedProfile {
    ResolvedProfile {
        layers: Vec::new(),
        merged: MergedProfile {
            modules: vec![module_name.to_string()],
            ..Default::default()
        },
    }
}

/// Collect known package manager names from the registry.
pub(in crate::cli) fn known_manager_names() -> Vec<String> {
    packages::all_package_managers()
        .iter()
        .map(|m| m.name().to_string())
        .collect()
}

/// Parse a `--file` value into (source_path, target_path).
/// - `<path>` without `:` → adopt in place: source=path, target=path
/// - `<source>:<target>` → explicit mapping
pub(in crate::cli) fn parse_file_spec(spec: &str) -> anyhow::Result<(PathBuf, PathBuf)> {
    // On Windows, paths like C:\foo contain colons that are NOT source:target separators.
    // A drive letter is a single ASCII letter followed by `:` and `\` or `/`.
    // We skip the first colon if it's part of a drive letter prefix.
    let split_pos = spec.char_indices().find_map(|(i, c)| {
        if c == ':' {
            // Skip if this colon is at position 1 and preceded by a single ASCII letter
            // (i.e., a Windows drive letter like C: or D:)
            if i == 1 && spec.as_bytes()[0].is_ascii_alphabetic() {
                return None;
            }
            Some(i)
        } else {
            None
        }
    });

    if let Some(pos) = split_pos {
        let source = &spec[..pos];
        let target = &spec[pos + 1..];
        // Target may also start with a drive letter — handle C:\path after the separator
        if source.is_empty() {
            anyhow::bail!("empty source in file spec: {}", spec);
        }
        if target.is_empty() {
            anyhow::bail!("empty target in file spec: {}", spec);
        }
        Ok((
            cfgd_core::expand_tilde(Path::new(source)),
            cfgd_core::expand_tilde(Path::new(target)),
        ))
    } else {
        let expanded = cfgd_core::expand_tilde(Path::new(spec));
        Ok((expanded.clone(), expanded))
    }
}

/// Adopt files: copy into `repo_dir`, symlink back from source location.
/// Returns `(basename, deploy_target)` pairs — basename is the filename in the repo,
/// deploy_target is where the file should be deployed on the machine.
pub(in crate::cli) fn copy_files_to_dir(
    file_specs: &[String],
    repo_dir: &Path,
) -> anyhow::Result<Vec<(String, PathBuf)>> {
    let mut results = Vec::new();
    for spec in file_specs {
        let (source, target) = parse_file_spec(spec)?;
        if !source.exists() {
            anyhow::bail!("File not found: {}", source.posix());
        }

        // Reject sources in system directories to prevent path traversal attacks.
        // module create --file copies the source then replaces it with a symlink,
        // so importing /etc/passwd would delete it and replace with a symlink.
        let canonical_source = source
            .canonicalize()
            .unwrap_or_else(|_| source.to_path_buf());
        // These prefixes are checked against both the original and canonical path.
        // /var is omitted here because on macOS /var/folders is the user temp
        // directory — tempfile crates produce paths under /var/folders/… which
        // must remain importable.  /var on Linux is covered via canonical_source
        // (Linux does not redirect /var, so original == canonical there).
        let forbidden_prefixes: &[&str] = &[
            "/etc",
            "/usr",
            "/bin",
            "/sbin",
            "/boot",
            "/sys",
            "/proc",
            "/lib",
            "/lib64",
            "/dev",
            "/snap",
            // macOS symlinks /etc → /private/etc; check canonical to catch traversal.
            "/private/etc",
        ];
        for prefix in forbidden_prefixes {
            if source.starts_with(prefix) || canonical_source.starts_with(prefix) {
                anyhow::bail!(
                    "Refusing to import '{}': source is in system directory {}",
                    source.posix(),
                    prefix
                );
            }
        }
        // Check /var against the canonical path only. On Linux canonical == original
        // so this catches system /var correctly. On macOS /var symlinks to
        // /private/var, so temp files (/var/folders/…) canonicalize to
        // /private/var/folders/… which does not start with /var — safe to allow.
        if canonical_source.starts_with("/var") {
            anyhow::bail!(
                "Refusing to import '{}': source is in system directory /var",
                source.posix()
            );
        }

        std::fs::create_dir_all(repo_dir)?;
        let file_name = source
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("Invalid file path: {}", source.posix()))?;
        let dest = repo_dir.join(file_name);
        if source.is_dir() {
            cfgd_core::copy_dir_recursive(&source, &dest)?;
        } else {
            std::fs::copy(&source, &dest)?;
        }
        // Symlink back from source location to repo copy so the user's
        // dotfile now points into the cfgd-managed directory.
        if source.exists() && !source.is_symlink() {
            if source.is_dir() {
                std::fs::remove_dir_all(&source)?;
            } else {
                std::fs::remove_file(&source)?;
            }
            cfgd_core::create_symlink(&dest, &source)?;
        }
        results.push((file_name.to_string_lossy().to_string(), target));
    }
    Ok(results)
}

/// Add a path to `.gitignore` in `config_dir` if not already present.
pub(in crate::cli) fn add_to_gitignore(config_dir: &Path, path: &str) -> anyhow::Result<()> {
    let gitignore = config_dir.join(".gitignore");
    let existing = if gitignore.exists() {
        std::fs::read_to_string(&gitignore)?
    } else {
        String::new()
    };
    // Check if already listed (exact line match)
    if existing.lines().any(|line| line.trim() == path) {
        return Ok(());
    }
    let mut content = existing;
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(path);
    content.push('\n');
    cfgd_core::atomic_write_str(&gitignore, &content)?;
    Ok(())
}

// --- Validation helpers ---

/// Validate a resource name (module or profile) for filesystem safety.
/// Allows alphanumeric, hyphen, underscore, and dot (but not leading dot).
pub(in crate::cli) fn validate_resource_name(name: &str, kind: &str) -> anyhow::Result<()> {
    if name.is_empty() {
        anyhow::bail!("{kind} name cannot be empty");
    }
    if name.len() > 128 {
        anyhow::bail!("{kind} name too long (max 128 characters)");
    }
    if name.starts_with('.') || name.starts_with('-') {
        anyhow::bail!("{kind} name cannot start with '.' or '-'");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        anyhow::bail!(
            "{kind} name '{}' contains invalid characters — use only alphanumeric, hyphen, underscore, or dot",
            name
        );
    }
    Ok(())
}

// --- Scan helpers ---

/// Scan a profiles/ directory and return sorted profile names.
pub(in crate::cli) fn scan_profile_names(profiles_dir: &Path) -> anyhow::Result<Vec<String>> {
    let mut names = Vec::new();
    cfgd_core::config::for_each_yaml_file(profiles_dir, |path| {
        if let Ok(doc) = config::load_profile(path) {
            names.push(doc.metadata.name);
        }
        Ok(())
    })?;
    names.sort();
    Ok(names)
}

/// Scan a modules/ directory and return sorted module names.
pub(in crate::cli) fn scan_module_names(modules_dir: &Path) -> anyhow::Result<Vec<String>> {
    let mut names = Vec::new();
    if modules_dir.exists() {
        for entry in std::fs::read_dir(modules_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir()
                && path.join("module.yaml").exists()
                && let Some(n) = entry.file_name().to_str()
            {
                names.push(n.to_string());
            }
        }
        names.sort();
    }
    Ok(names)
}

// --- Registry / state / editor helpers ---

/// Build a HashMap of manager name → &dyn PackageManager from the registry.
pub(in crate::cli) fn managers_map(
    registry: &ProviderRegistry,
) -> std::collections::HashMap<String, &dyn cfgd_core::providers::PackageManager> {
    registry
        .package_managers
        .iter()
        .map(|m| (m.name().to_string(), m.as_ref()))
        .collect()
}

pub(in crate::cli) fn module_state_map(
    state: &cfgd_core::state::StateStore,
) -> std::collections::HashMap<String, cfgd_core::state::ModuleStateRecord> {
    state
        .module_states()
        .unwrap_or_default()
        .into_iter()
        .map(|s| (s.module_name.clone(), s))
        .collect()
}

pub(in crate::cli) fn open_in_editor(path: &Path, printer: &Printer) -> anyhow::Result<()> {
    let editor = std::env::var("EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .unwrap_or_else(|_| "vi".to_string());

    let status = std::process::Command::new(&editor)
        .arg(path)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .map_err(|e| anyhow::anyhow!("Failed to open editor '{}': {}", editor, e))?;

    if !status.success() {
        printer.status_simple(
            Role::Warn,
            format!("Editor '{}' exited with non-zero status", editor),
        );
    }
    Ok(())
}

pub(in crate::cli) fn config_dir(cli: &Cli) -> PathBuf {
    cli.config
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

pub(in crate::cli) fn profiles_dir(cli: &Cli) -> PathBuf {
    config_dir(cli).join("profiles")
}

/// List sorted YAML file stems in a directory (e.g. "base" from "base.yaml").
/// Returns an empty vec if the directory doesn't exist.
pub(in crate::cli) fn list_yaml_stems(dir: &Path) -> anyhow::Result<Vec<String>> {
    let mut names = Vec::new();
    cfgd_core::config::for_each_yaml_file(dir, |path| {
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            names.push(stem.to_string());
        }
        Ok(())
    })?;
    names.sort();
    Ok(names)
}

/// Emit the structured no-config error doc and return the typed `ConfigNotFound`
/// error so every command's missing-config path exits with the same code (3) and
/// names the path, matching plan/status/apply. `main.rs` downcasts the returned
/// `CfgdError` to map it onto `ExitCode::NoConfig`.
pub(in crate::cli) fn no_config_error(printer: &Printer, config_path: &Path) -> anyhow::Error {
    printer.emit(cfgd_core::output::error_doc(
        &config_path.display().to_string(),
        "no_config",
        format!("config file not found: {}", config_path.display_posix()),
        serde_json::json!({ "path": cfgd_core::to_posix_string(config_path) }),
    ));
    cfgd_core::errors::CfgdError::Config(cfgd_core::errors::ConfigError::NotFound {
        path: config_path.to_path_buf(),
    })
    .into()
}

/// Resolve profile name from explicit name or default to active profile.
pub(in crate::cli) fn resolve_profile_name(
    cli: &Cli,
    printer: &Printer,
    name: Option<&str>,
) -> anyhow::Result<String> {
    if let Some(n) = name {
        return Ok(n.to_string());
    }
    // Default to active profile
    let config_path = &cli.config;
    if !config_path.exists() {
        return Err(no_config_error(printer, config_path));
    }
    let cfg = config::load_config(config_path)?;
    if let Some(ref profile_override) = cli.profile {
        Ok(profile_override.clone())
    } else {
        Ok(cfg.active_profile()?.to_string())
    }
}

pub(in crate::cli) fn default_device_id() -> String {
    cfgd_core::hostname_string()
}

pub(in crate::cli) fn set_nested_yaml_value(
    root: &mut serde_yaml::Value,
    path: &str,
    value: &serde_yaml::Value,
) -> anyhow::Result<()> {
    let parts: Vec<&str> = path.split('.').collect();
    let mut current = root;

    for (i, part) in parts.iter().enumerate() {
        if i == parts.len() - 1 {
            // Last part: set the value
            if let Some(mapping) = current.as_mapping_mut() {
                mapping.insert(serde_yaml::Value::String(part.to_string()), value.clone());
            }
        } else {
            // Intermediate part: navigate or create
            let mapping = current
                .as_mapping_mut()
                .ok_or_else(|| anyhow::anyhow!("expected mapping at '{}'", part))?;
            current = mapping
                .entry(serde_yaml::Value::String(part.to_string()))
                .or_insert(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
        }
    }

    Ok(())
}

// --- Plan integration with sources (Phase 9) ---

pub(in crate::cli) fn compose_with_sources(
    cli: &Cli,
    cfg: &config::CfgdConfig,
    local_resolved: &ResolvedProfile,
    printer: &Printer,
) -> anyhow::Result<composition::CompositionResult> {
    if cfg.spec.sources.is_empty() {
        // No sources, return local profile as-is
        return Ok(composition::CompositionResult {
            resolved: local_resolved.clone(),
            conflicts: Vec::new(),
            source_env: std::collections::HashMap::new(),
            source_commits: std::collections::HashMap::new(),
        });
    }

    let cache_dir = source_cache_dir(cli)?;
    let mut mgr = SourceManager::new(&cache_dir);
    mgr.set_allow_unsigned(cfg.spec.security.as_ref().is_some_and(|s| s.allow_unsigned));
    mgr.load_sources(&cfg.spec.sources, printer)?;

    let mut inputs = Vec::new();
    for source_spec in &cfg.spec.sources {
        if let Some(cached) = mgr.get(&source_spec.name) {
            // Load source profile layers
            let mut layers = Vec::new();
            if let Some(ref profile_name) = source_spec.subscription.profile {
                let profiles_dir = mgr.source_profiles_dir(&source_spec.name)?;
                if profiles_dir.exists() {
                    match config::resolve_profile(profile_name, &profiles_dir) {
                        Ok(resolved) => {
                            layers = resolved.layers;
                        }
                        Err(e) => {
                            printer.status_simple(
                                Role::Warn,
                                format!(
                                    "Failed to resolve profile '{}' from source '{}': {}",
                                    profile_name, source_spec.name, e
                                ),
                            );
                        }
                    }
                }
            }

            // Validate security constraints
            for layer in &layers {
                if let Err(e) = composition::validate_constraints(
                    &source_spec.name,
                    &cached.manifest.spec.policy.constraints,
                    &layer.spec,
                ) {
                    printer.status_simple(
                        Role::Fail,
                        format!("Security violation in source '{}': {}", source_spec.name, e),
                    );
                    continue;
                }
            }

            // Check if local config overrides any locked resources from this source
            if let Err(e) = composition::check_locked_violations(
                &source_spec.name,
                &cached.manifest.spec.policy.locked,
                &local_resolved.merged,
            ) {
                printer.status_simple(
                    Role::Warn,
                    format!(
                        "Locked resource conflict with source '{}': {}",
                        source_spec.name, e
                    ),
                );
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

    let mut result = composition::compose(local_resolved, &inputs)?;

    // Collect source commit hashes for record_source_apply linkage
    for source_spec in &cfg.spec.sources {
        if let Some(cached) = mgr.get(&source_spec.name)
            && let Some(ref commit) = cached.last_commit
        {
            result
                .source_commits
                .insert(source_spec.name.clone(), commit.clone());
        }
    }

    // Display conflicts
    if !result.conflicts.is_empty() {
        let guard = printer.section("Source Conflicts");
        for conflict in &result.conflicts {
            match conflict.resolution_type {
                composition::ResolutionType::Locked => {
                    guard.status_simple(Role::Warn, &conflict.details);
                }
                composition::ResolutionType::Required => {
                    guard.status_simple(Role::Info, &conflict.details);
                }
                composition::ResolutionType::Rejected => {
                    guard.status_simple(Role::Info, &conflict.details);
                }
                composition::ResolutionType::Override => {
                    guard.status_simple(Role::Info, &conflict.details);
                }
                composition::ResolutionType::Default => {}
            }
        }
        drop(guard);

        // Persist conflicts to state
        if let Ok(state) = open_state_store(cli.state_dir.as_deref()) {
            for conflict in &result.conflicts {
                if let Err(e) = state.record_source_conflict(
                    &conflict.winning_source,
                    "composition",
                    &conflict.resource_id,
                    conflict.resolution_type.label(),
                    Some(&conflict.details),
                ) {
                    tracing::warn!(
                        error = %e,
                        winning_source = %conflict.winning_source,
                        resource_id = %conflict.resource_id,
                        "failed to persist source conflict to state store; conflict history may be incomplete",
                    );
                }
            }
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serial_test::serial;
    use tempfile::tempdir;

    use super::*;
    use cfgd_core::output::{OutputFormat, Printer, Verbosity};
    use cfgd_core::test_helpers::EnvVarGuard;

    // ---------------------------------------------------------------------------
    // Helpers shared across tests
    // ---------------------------------------------------------------------------

    fn make_cli(config: PathBuf) -> Cli {
        Cli {
            config,
            profile: None,
            verbose: 0,
            quiet: true,
            no_color: true,
            output: OutputFormatArg(OutputFormat::Table),
            jsonpath: None,
            state_dir: None,
            command: None,
        }
    }

    const CONFIG_YAML: &str = "apiVersion: cfgd.io/v1alpha1\n\
                               kind: Config\n\
                               metadata:\n  name: t\n\
                               spec:\n  profile: default\n";

    const PROFILE_YAML: &str = "apiVersion: cfgd.io/v1alpha1\n\
                                kind: Profile\n\
                                metadata:\n  name: default\n\
                                spec: {}\n";

    fn quiet_printer() -> Printer {
        Printer::new(Verbosity::Quiet)
    }

    // ---------------------------------------------------------------------------
    // parse_package_flag
    // ---------------------------------------------------------------------------

    #[test]
    fn parse_package_flag_bare_package_has_no_manager() {
        let (mgr, pkg) = parse_package_flag("ripgrep", &["brew", "apt"]);
        assert_eq!(mgr, None);
        assert_eq!(pkg, "ripgrep");
    }

    #[test]
    fn parse_package_flag_known_manager_prefix_splits() {
        let (mgr, pkg) = parse_package_flag("brew:ripgrep", &["brew", "apt"]);
        assert_eq!(mgr.as_deref(), Some("brew"));
        assert_eq!(pkg, "ripgrep");
    }

    #[test]
    fn parse_package_flag_unknown_manager_prefix_is_bare() {
        let (mgr, pkg) = parse_package_flag("cargo:ripgrep", &["brew", "apt"]);
        assert_eq!(mgr, None);
        assert_eq!(pkg, "cargo:ripgrep");
    }

    #[test]
    fn parse_package_flag_empty_prefix_is_bare() {
        // ":ripgrep" has an empty prefix — treat as bare package name.
        let (mgr, pkg) = parse_package_flag(":ripgrep", &["brew"]);
        assert_eq!(mgr, None);
        assert_eq!(pkg, ":ripgrep");
    }

    #[test]
    fn parse_package_flag_empty_suffix_is_bare() {
        // "brew:" has an empty suffix — treat as bare package name.
        let (mgr, pkg) = parse_package_flag("brew:", &["brew"]);
        assert_eq!(mgr, None);
        assert_eq!(pkg, "brew:");
    }

    #[test]
    fn parse_package_flag_no_known_managers_always_bare() {
        let (mgr, pkg) = parse_package_flag("brew:ripgrep", &[]);
        assert_eq!(mgr, None);
        assert_eq!(pkg, "brew:ripgrep");
    }

    // ---------------------------------------------------------------------------
    // empty_resolved_profile
    // ---------------------------------------------------------------------------

    #[test]
    fn empty_resolved_profile_contains_only_named_module() {
        let rp = empty_resolved_profile("mymod");
        assert!(rp.layers.is_empty());
        assert_eq!(rp.merged.modules, vec!["mymod".to_string()]);
        // All other merged fields are default-empty.
        assert!(rp.merged.env.is_empty());
    }

    // ---------------------------------------------------------------------------
    // known_manager_names
    // ---------------------------------------------------------------------------

    #[test]
    fn known_manager_names_returns_non_empty_list() {
        let names = known_manager_names();
        assert!(
            !names.is_empty(),
            "expected at least one package manager registered"
        );
        // Every name must be a non-empty string.
        for name in &names {
            assert!(!name.is_empty(), "manager name must not be empty");
        }
    }

    // ---------------------------------------------------------------------------
    // parse_file_spec
    // ---------------------------------------------------------------------------

    #[test]
    fn parse_file_spec_single_path_returns_same_src_and_dst() {
        let (src, dst) = parse_file_spec("/home/user/.bashrc").unwrap();
        assert_eq!(src, PathBuf::from("/home/user/.bashrc"));
        assert_eq!(dst, PathBuf::from("/home/user/.bashrc"));
    }

    #[test]
    fn parse_file_spec_src_colon_dst_splits_correctly() {
        let (src, dst) = parse_file_spec("/tmp/a.txt:/etc/a.txt").unwrap();
        assert_eq!(src, PathBuf::from("/tmp/a.txt"));
        assert_eq!(dst, PathBuf::from("/etc/a.txt"));
    }

    #[test]
    fn parse_file_spec_windows_drive_letter_no_split() {
        // C:\foo has a colon at position 1 after an ASCII letter — must NOT split.
        let (src, dst) = parse_file_spec("C:\\foo\\bar").unwrap();
        assert_eq!(src, PathBuf::from("C:\\foo\\bar"));
        assert_eq!(dst, PathBuf::from("C:\\foo\\bar"));
    }

    #[test]
    fn parse_file_spec_windows_drive_as_source_with_target() {
        // "C:\foo:/dest" — second colon is the src:dst separator.
        let (src, dst) = parse_file_spec("C:\\foo:/dest").unwrap();
        assert_eq!(src, PathBuf::from("C:\\foo"));
        assert_eq!(dst, PathBuf::from("/dest"));
    }

    #[test]
    fn parse_file_spec_empty_source_errors() {
        let err = parse_file_spec(":/dst").unwrap_err();
        assert!(
            err.to_string().contains("empty source"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_file_spec_empty_target_errors() {
        let err = parse_file_spec("/src:").unwrap_err();
        assert!(
            err.to_string().contains("empty target"),
            "unexpected error: {err}"
        );
    }

    // ---------------------------------------------------------------------------
    // validate_resource_name
    // ---------------------------------------------------------------------------

    #[test]
    fn validate_resource_name_accepts_valid_names() {
        for name in &["mymod", "my-mod", "my_mod", "my.mod", "mod123", "m"] {
            validate_resource_name(name, "module")
                .unwrap_or_else(|e| panic!("rejected valid name '{name}': {e}"));
        }
    }

    #[test]
    fn validate_resource_name_rejects_empty() {
        let err = validate_resource_name("", "module").unwrap_err();
        assert!(err.to_string().contains("cannot be empty"), "{err}");
    }

    #[test]
    fn validate_resource_name_rejects_leading_dot() {
        let err = validate_resource_name(".hidden", "module").unwrap_err();
        assert!(
            err.to_string().contains("cannot start with"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn validate_resource_name_rejects_leading_dash() {
        let err = validate_resource_name("-start", "module").unwrap_err();
        assert!(
            err.to_string().contains("cannot start with"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn validate_resource_name_rejects_invalid_chars() {
        let err = validate_resource_name("my mod", "module").unwrap_err();
        assert!(
            err.to_string().contains("invalid characters"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn validate_resource_name_rejects_name_too_long() {
        let long = "a".repeat(129);
        let err = validate_resource_name(&long, "module").unwrap_err();
        assert!(err.to_string().contains("too long"), "unexpected: {err}");
    }

    // ---------------------------------------------------------------------------
    // set_nested_yaml_value
    // ---------------------------------------------------------------------------

    #[test]
    fn set_nested_yaml_value_sets_top_level_key() {
        let mut root = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        set_nested_yaml_value(
            &mut root,
            "name",
            &serde_yaml::Value::String("alice".to_string()),
        )
        .unwrap();
        assert_eq!(root["name"], serde_yaml::Value::String("alice".to_string()));
    }

    #[test]
    fn set_nested_yaml_value_creates_intermediate_maps() {
        let mut root = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        set_nested_yaml_value(
            &mut root,
            "a.b.c",
            &serde_yaml::Value::String("deep".to_string()),
        )
        .unwrap();
        assert_eq!(
            root["a"]["b"]["c"],
            serde_yaml::Value::String("deep".to_string())
        );
    }

    #[test]
    fn set_nested_yaml_value_overwrites_existing_key() {
        let mut root: serde_yaml::Value = serde_yaml::from_str("key: old").unwrap();
        set_nested_yaml_value(
            &mut root,
            "key",
            &serde_yaml::Value::String("new".to_string()),
        )
        .unwrap();
        assert_eq!(root["key"], serde_yaml::Value::String("new".to_string()));
    }

    #[test]
    fn set_nested_yaml_value_two_level_path() {
        let mut root: serde_yaml::Value = serde_yaml::from_str("spec:\n  active: old").unwrap();
        set_nested_yaml_value(
            &mut root,
            "spec.active",
            &serde_yaml::Value::String("new".to_string()),
        )
        .unwrap();
        assert_eq!(
            root["spec"]["active"],
            serde_yaml::Value::String("new".to_string())
        );
    }

    // ---------------------------------------------------------------------------
    // default_device_id
    // ---------------------------------------------------------------------------

    #[test]
    fn default_device_id_returns_non_empty_string() {
        let id = default_device_id();
        assert!(!id.is_empty(), "device id must not be empty");
    }

    // ---------------------------------------------------------------------------
    // config_dir / profiles_dir
    // ---------------------------------------------------------------------------

    #[test]
    fn config_dir_returns_parent_of_config_file() {
        let tmp = tempdir().unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        let cli = make_cli(config_path.clone());
        assert_eq!(config_dir(&cli), tmp.path());
    }

    #[test]
    fn profiles_dir_is_profiles_subdir_of_config_dir() {
        let tmp = tempdir().unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        let cli = make_cli(config_path);
        assert_eq!(profiles_dir(&cli), tmp.path().join("profiles"));
    }

    // ---------------------------------------------------------------------------
    // add_to_gitignore
    // ---------------------------------------------------------------------------

    #[test]
    fn add_to_gitignore_creates_file_and_adds_entry() {
        let tmp = tempdir().unwrap();
        add_to_gitignore(tmp.path(), "secrets/").unwrap();
        let contents = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
        assert!(contents.contains("secrets/"), "entry missing: {contents}");
    }

    #[test]
    fn add_to_gitignore_is_idempotent() {
        let tmp = tempdir().unwrap();
        add_to_gitignore(tmp.path(), "secrets/").unwrap();
        add_to_gitignore(tmp.path(), "secrets/").unwrap();
        let contents = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
        let count = contents.lines().filter(|l| l.trim() == "secrets/").count();
        assert_eq!(count, 1, "entry written more than once: {contents}");
    }

    #[test]
    fn add_to_gitignore_appends_to_existing_file() {
        let tmp = tempdir().unwrap();
        let gitignore = tmp.path().join(".gitignore");
        std::fs::write(&gitignore, "target/\n").unwrap();
        add_to_gitignore(tmp.path(), "secrets/").unwrap();
        let contents = std::fs::read_to_string(&gitignore).unwrap();
        assert!(contents.contains("target/"), "original entry lost");
        assert!(contents.contains("secrets/"), "new entry missing");
    }

    // ---------------------------------------------------------------------------
    // list_yaml_stems
    // ---------------------------------------------------------------------------

    #[test]
    fn list_yaml_stems_empty_dir_returns_empty() {
        let tmp = tempdir().unwrap();
        let stems = list_yaml_stems(tmp.path()).unwrap();
        assert!(stems.is_empty());
    }

    #[test]
    fn list_yaml_stems_returns_sorted_stems() {
        let tmp = tempdir().unwrap();
        std::fs::write(tmp.path().join("zebra.yaml"), "").unwrap();
        std::fs::write(tmp.path().join("alpha.yaml"), "").unwrap();
        std::fs::write(tmp.path().join("middle.yml"), "").unwrap();
        let stems = list_yaml_stems(tmp.path()).unwrap();
        assert_eq!(stems, vec!["alpha", "middle", "zebra"]);
    }

    #[test]
    fn list_yaml_stems_ignores_non_yaml_files() {
        let tmp = tempdir().unwrap();
        std::fs::write(tmp.path().join("notes.txt"), "").unwrap();
        std::fs::write(tmp.path().join("config.yaml"), "").unwrap();
        let stems = list_yaml_stems(tmp.path()).unwrap();
        assert_eq!(stems, vec!["config"]);
    }

    #[test]
    fn list_yaml_stems_missing_dir_returns_empty() {
        let tmp = tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist");
        let stems = list_yaml_stems(&missing).unwrap();
        assert!(stems.is_empty());
    }

    // ---------------------------------------------------------------------------
    // scan_module_names
    // ---------------------------------------------------------------------------

    #[test]
    fn scan_module_names_empty_dir_returns_empty() {
        let tmp = tempdir().unwrap();
        let names = scan_module_names(tmp.path()).unwrap();
        assert!(names.is_empty());
    }

    #[test]
    fn scan_module_names_finds_modules_with_module_yaml() {
        let tmp = tempdir().unwrap();
        let mod_a = tmp.path().join("alpha");
        let mod_b = tmp.path().join("beta");
        std::fs::create_dir_all(&mod_a).unwrap();
        std::fs::create_dir_all(&mod_b).unwrap();
        std::fs::write(mod_a.join("module.yaml"), "").unwrap();
        std::fs::write(mod_b.join("module.yaml"), "").unwrap();
        // dir without module.yaml must NOT be included
        std::fs::create_dir_all(tmp.path().join("not-a-module")).unwrap();
        let names = scan_module_names(tmp.path()).unwrap();
        assert_eq!(names, vec!["alpha", "beta"]);
    }

    #[test]
    fn scan_module_names_missing_dir_returns_empty() {
        let tmp = tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist");
        let names = scan_module_names(&missing).unwrap();
        assert!(names.is_empty());
    }

    // ---------------------------------------------------------------------------
    // copy_files_to_dir
    // ---------------------------------------------------------------------------

    #[test]
    fn copy_files_to_dir_copies_file_and_symlinks_back() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();
        let src_file = src_dir.path().join("dotfile.txt");
        std::fs::write(&src_file, "hello").unwrap();

        let spec = src_file.to_string_lossy().to_string();
        let results = copy_files_to_dir(&[spec], dst_dir.path()).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "dotfile.txt");
        assert!(dst_dir.path().join("dotfile.txt").exists());
        assert!(
            src_file.is_symlink(),
            "source should have been replaced with a symlink"
        );
        let content = std::fs::read_to_string(&src_file).unwrap();
        assert_eq!(content, "hello");
    }

    #[test]
    fn copy_files_to_dir_missing_source_errors() {
        let dst_dir = tempdir().unwrap();
        let err = copy_files_to_dir(&["/tmp/cfgd-nonexistent-9999.txt".into()], dst_dir.path())
            .unwrap_err();
        assert!(
            err.to_string().contains("File not found"),
            "unexpected: {err}"
        );
    }

    // ---------------------------------------------------------------------------
    // open_in_editor — requires EDITOR env var; must be serial
    // ---------------------------------------------------------------------------

    #[test]
    #[serial]
    fn open_in_editor_succeeds_when_editor_exits_zero() {
        let tmp = tempdir().unwrap();
        let file = tmp.path().join("edit_me.yaml");
        std::fs::write(&file, "").unwrap();
        let _editor = EnvVarGuard::set("EDITOR", "true");
        let printer = quiet_printer();
        open_in_editor(&file, &printer).unwrap();
        // No panic and no error — that's the contract for a zero-exit editor.
    }

    #[test]
    #[serial]
    fn open_in_editor_nonzero_exit_prints_warn_but_does_not_error() {
        let tmp = tempdir().unwrap();
        let file = tmp.path().join("edit_me.yaml");
        std::fs::write(&file, "").unwrap();
        // `false` always exits 1.
        let _editor = EnvVarGuard::set("EDITOR", "false");
        let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
        // Must return Ok even when editor exits non-zero (only warns).
        open_in_editor(&file, &printer).unwrap();
        drop(printer);
        let output = buf.lock().unwrap();
        assert!(
            output.contains("non-zero"),
            "expected warn about non-zero exit, got: {output}"
        );
    }

    // ---------------------------------------------------------------------------
    // copy_files_to_dir — directory source path
    // ---------------------------------------------------------------------------

    #[test]
    fn copy_files_to_dir_copies_directory_and_symlinks_back() {
        let src_base = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create a source directory with a file inside.
        let src_subdir = src_base.path().join("mydir");
        std::fs::create_dir_all(&src_subdir).unwrap();
        std::fs::write(src_subdir.join("inner.txt"), "inner").unwrap();

        let spec = src_subdir.to_string_lossy().to_string();
        let results = copy_files_to_dir(&[spec], dst_dir.path()).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "mydir");
        assert!(dst_dir.path().join("mydir").is_dir());
        assert!(
            src_subdir.is_symlink(),
            "source dir should be a symlink after copy"
        );
    }

    // ---------------------------------------------------------------------------
    // resolve_profile_name
    // ---------------------------------------------------------------------------

    #[test]
    fn resolve_profile_name_returns_explicit_name_without_reading_config() {
        // When an explicit name is provided, it is returned immediately —
        // the config file need not exist.
        let tmp = tempdir().unwrap();
        let cli = make_cli(tmp.path().join("nonexistent.yaml"));
        let name = resolve_profile_name(&cli, &quiet_printer(), Some("staging")).unwrap();
        assert_eq!(name, "staging");
    }

    #[test]
    fn resolve_profile_name_errors_when_no_config_and_no_explicit_name() {
        let tmp = tempdir().unwrap();
        let config_path = tmp.path().join("nonexistent.yaml");
        let cli = make_cli(config_path.clone());
        let err = resolve_profile_name(&cli, &quiet_printer(), None).unwrap_err();
        let cfgd_err = err
            .downcast_ref::<cfgd_core::errors::CfgdError>()
            .expect("typed CfgdError");
        assert!(
            matches!(
                cfgd_err,
                cfgd_core::errors::CfgdError::Config(
                    cfgd_core::errors::ConfigError::NotFound { .. }
                )
            ),
            "expected ConfigError::NotFound, got: {cfgd_err}"
        );
        assert!(
            err.to_string().contains("config file not found")
                && err.to_string().contains("nonexistent.yaml"),
            "error should name the path: {err}"
        );
    }

    #[test]
    fn resolve_profile_name_returns_cli_profile_override_when_set() {
        let tmp = tempdir().unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(&config_path, CONFIG_YAML).unwrap();
        let mut cli = make_cli(config_path);
        cli.profile = Some("override-profile".to_string());
        // No explicit name passed → should fall through to cli.profile.
        let name = resolve_profile_name(&cli, &quiet_printer(), None).unwrap();
        assert_eq!(name, "override-profile");
    }

    // ---------------------------------------------------------------------------
    // managers_map
    // ---------------------------------------------------------------------------

    #[test]
    fn managers_map_empty_registry_returns_empty_map() {
        let registry = ProviderRegistry::new();
        let map = managers_map(&registry);
        assert!(map.is_empty());
    }

    #[test]
    fn managers_map_keys_match_manager_names() {
        let mut registry = ProviderRegistry::new();
        registry.package_managers = packages::all_package_managers();
        let map = managers_map(&registry);
        assert!(
            !map.is_empty(),
            "expected managers from all_package_managers"
        );
        // Every key must equal the corresponding manager's own name().
        for (name, mgr) in &map {
            assert_eq!(name, mgr.name(), "key mismatch for manager '{name}'");
        }
    }

    // ---------------------------------------------------------------------------
    // module_state_map
    // ---------------------------------------------------------------------------

    #[test]
    fn module_state_map_empty_store_returns_empty_map() {
        let state = cfgd_core::state::StateStore::open_in_memory().unwrap();
        let map = module_state_map(&state);
        assert!(map.is_empty());
    }

    // ---------------------------------------------------------------------------
    // compose_with_sources — no-sources fast path
    // ---------------------------------------------------------------------------

    #[test]
    fn compose_with_sources_no_sources_returns_local_profile_unchanged() {
        let tmp = tempdir().unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(&config_path, CONFIG_YAML).unwrap();
        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(profiles_dir.join("default.yaml"), PROFILE_YAML).unwrap();

        let cli = make_cli(config_path.clone());
        let cfg = config::load_config(&config_path).unwrap();
        let local = empty_resolved_profile("my-module");
        let printer = quiet_printer();

        let result = compose_with_sources(&cli, &cfg, &local, &printer).unwrap();

        // No sources → resolved must equal the local profile we passed in.
        assert_eq!(result.resolved.merged.modules, local.merged.modules);
        assert!(result.conflicts.is_empty());
        assert!(result.source_env.is_empty());
        assert!(result.source_commits.is_empty());
    }

    /// Build a minimal local git repo that acts as a cfgd source.
    /// The source's `<profile_name>.yaml` profile declares a module named
    /// `source-module` so the composition can be asserted on a non-empty
    /// contribution from the source layer.
    fn create_local_source_repo(root: &std::path::Path, profile_name: &str) -> PathBuf {
        let repo_dir = root.join("source-repo");
        std::fs::create_dir_all(&repo_dir).unwrap();

        cfgd_core::git_cmd_local()
            .args(["init", "-b", "master"])
            .current_dir(&repo_dir)
            .output()
            .unwrap();
        cfgd_core::git_cmd_local()
            .args(["config", "user.email", "test@example.com"])
            .current_dir(&repo_dir)
            .output()
            .unwrap();
        cfgd_core::git_cmd_local()
            .args(["config", "user.name", "Test"])
            .current_dir(&repo_dir)
            .output()
            .unwrap();

        let manifest = format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: ConfigSource\nmetadata:\n  name: test-src\nspec:\n  provides:\n    profiles:\n      - {profile_name}\n"
        );
        std::fs::write(repo_dir.join("cfgd-source.yaml"), &manifest).unwrap();

        let profile_yaml = format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: {profile_name}\nspec:\n  modules:\n    - source-module\n"
        );
        std::fs::create_dir_all(repo_dir.join("profiles")).unwrap();
        std::fs::write(
            repo_dir
                .join("profiles")
                .join(format!("{profile_name}.yaml")),
            &profile_yaml,
        )
        .unwrap();

        cfgd_core::git_cmd_local()
            .args(["add", "."])
            .current_dir(&repo_dir)
            .output()
            .unwrap();
        cfgd_core::git_cmd_local()
            .args(["commit", "-m", "init"])
            .current_dir(&repo_dir)
            .output()
            .unwrap();

        repo_dir
    }

    #[test]
    #[serial]
    fn compose_with_sources_with_local_source_merges_source_profile() {
        let tmp = tempdir().unwrap();
        let source_repo = create_local_source_repo(tmp.path(), "team");

        // cfgd.yaml with one local source that selects the "team" profile.
        // The source's team.yaml declares the `source-module` module, which
        // the composition must merge into the resolved profile.
        let config_yaml = format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n  sources:\n    - name: test-src\n      origin:\n        type: Git\n        url: {}\n        branch: master\n      subscription:\n        profile: team\n",
            source_repo.display()
        );
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(&config_path, &config_yaml).unwrap();
        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(profiles_dir.join("default.yaml"), PROFILE_YAML).unwrap();

        let _allow = EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
        let mut cli = make_cli(config_path.clone());
        cli.state_dir = Some(tmp.path().join("state"));

        let cfg = config::load_config(&config_path).unwrap();
        let local = empty_resolved_profile("my-module");
        let printer = quiet_printer();

        let result = compose_with_sources(&cli, &cfg, &local, &printer).unwrap();

        // Source-commit field must be populated — proves the source was
        // cloned, parsed, and tracked by the composition.
        assert!(
            result.source_commits.contains_key("test-src"),
            "expected source_commits to record 'test-src', got: {:?}",
            result.source_commits
        );
        let commit = &result.source_commits["test-src"];
        assert_eq!(
            commit.len(),
            40,
            "expected 40-char commit SHA, got '{commit}'"
        );

        // Behavior assertion: the source's team.yaml declares `source-module`,
        // so the merged profile must contain it alongside the local
        // `my-module`.
        assert!(
            result
                .resolved
                .merged
                .modules
                .contains(&"source-module".to_string()),
            "merged modules missing source contribution: {:?}",
            result.resolved.merged.modules
        );
    }
}
