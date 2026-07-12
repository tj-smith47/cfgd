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
    let resolved = match config::resolve_profile(&profile_name, &profiles_dir(cli)) {
        Ok(resolved) => resolved,
        Err(e) => return Err(decorate_profile_not_found(cli, &cfg, &profile_name, e)),
    };
    Ok((cfg, profile_name, resolved))
}

/// Turn a bare `ProfileNotFound` into an actionable error when the requested
/// profile is actually delivered by a subscribed source. cfgd's composition
/// model requires the active/selected profile to be a LOCAL profile; a
/// source-delivered profile is a building block you wrap by setting that
/// source's `subscription.profile`. Without this, the user sees only "profile
/// not found" with no clue the name exists remotely.
///
/// Best-effort and side-effect-free: it scans each subscribed source's on-disk
/// profile cache (no network, no signature verification). Any failure to
/// classify — including a non-`ProfileNotFound` error or no providing source —
/// returns the original error unchanged, preserving the typed exit code.
fn decorate_profile_not_found(
    cli: &Cli,
    cfg: &CfgdConfig,
    profile_name: &str,
    original: cfgd_core::errors::CfgdError,
) -> anyhow::Error {
    use cfgd_core::errors::{CfgdError, ConfigError};

    // Only the not-found case (typo OR source-delivered) is decoratable; a
    // circular-inheritance or parse error must surface as-is.
    if !matches!(
        &original,
        CfgdError::Config(ConfigError::ProfileNotFound { .. })
    ) {
        return original.into();
    }

    let providers = sources_providing_profile(cli, cfg, profile_name);
    if providers.is_empty() {
        // A plain typo: no source delivers this name. Bare ProfileNotFound, exit 6.
        return original.into();
    }

    let providers_list = providers.join(", ");
    // `--config` may name a DIRECTORY (the default resolves a dir, then joins the
    // config filename); normalize to the concrete file the user must open.
    let config_file = cfgd_core::config::resolve_config_path(&cli.config);

    // Prose stays in hints (one `→` line each); the YAML wrap goes in a tight,
    // copy-pasteable code block. Schema: spec.sources[].subscription.profile
    // wires the source profile in (see docs/sources.md); spec.profile is the
    // local active profile.
    let hints = vec![
        cfgd_core::output::collapse_to_subject_line(format!(
            "Profile '{profile_name}' is delivered by source(s): {providers_list}. The active/selected profile must be a LOCAL profile; wrap the source profile in one."
        )),
        cfgd_core::output::collapse_to_subject_line(format!(
            "Set the source's subscription.profile in {}:",
            config_file.posix()
        )),
    ];

    let code_block = vec![
        "spec:".to_string(),
        "  sources:".to_string(),
        format!("    - name: {}", providers[0]),
        "      subscription:".to_string(),
        format!("        profile: {profile_name}"),
    ];

    let extras = serde_json::json!({
        "profile": profile_name,
        "sources": providers,
    });

    crate::cli::cli_error_ctx_with_hints_and_block(
        original.into(),
        profile_name,
        "profile_source_delivered",
        format!("profile not found: {profile_name}"),
        extras,
        hints,
        code_block,
    )
}

/// Names of subscribed sources whose on-disk profile cache contains a profile
/// named `profile_name`, in either manifest form (canonical bundle or legacy
/// flat). Best-effort: a source with no cache, an unreadable dir, or no match
/// is simply omitted (never an error). An ambiguous name still counts — the
/// source does deliver that profile, however malformed its layout.
fn sources_providing_profile(cli: &Cli, cfg: &CfgdConfig, profile_name: &str) -> Vec<String> {
    let Ok(cache_dir) = source_cache_dir(cli) else {
        return Vec::new();
    };
    let mgr = SourceManager::new(&cache_dir);
    cfg.spec
        .sources
        .iter()
        .filter(|spec| {
            let dir = mgr.cached_profiles_dir(&spec.name);
            cfgd_core::config::scan_profiles_tolerant(&dir)
                .map(|entries| entries.iter().any(|e| e.name() == profile_name))
                .unwrap_or(false)
        })
        .map(|spec| spec.name.clone())
        .collect()
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

/// Best-effort workflow regeneration after a completed mutation: the
/// mutation already succeeded, so a regeneration failure (e.g. an unrelated
/// ambiguous profile on disk) warns instead of flipping the exit non-zero.
pub(in crate::cli) fn update_workflow_best_effort(cli: &Cli, printer: &Printer) {
    if let Err(e) = maybe_update_workflow(cli, printer) {
        printer.status_simple(
            Role::Warn,
            format!(
                "workflow regeneration failed: {}",
                cfgd_core::output::collapse_to_subject_line(&*e)
            ),
        );
    }
}

// --- Scan helpers ---

/// Scan a profiles/ directory and return sorted profile names.
pub(in crate::cli) fn scan_profile_names(
    profiles_dir: &Path,
    printer: &Printer,
) -> anyhow::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in cfgd_core::config::scan_profiles_tolerant(profiles_dir)
        .map_err(cfgd_core::errors::CfgdError::Config)?
    {
        let found = match entry {
            cfgd_core::config::ProfileScanEntry::Found(found) => found,
            // Ambiguity fails closed only for direct operations on that
            // profile; here it gets the same warn-and-skip treatment as an
            // unparseable manifest so unrelated work can continue.
            cfgd_core::config::ProfileScanEntry::Ambiguous { name, error, .. } => {
                printer.status_simple(
                    Role::Warn,
                    format!(
                        "Skipping profile '{}': {}",
                        name,
                        cfgd_core::output::collapse_to_subject_line(&error)
                    ),
                );
                continue;
            }
        };
        // Scanned stems flow into generated-workflow grep patterns and bare
        // YAML matrix lines — an invalid on-disk name (quote, newline, …)
        // would corrupt the generated file silently, so gate it here.
        if let Err(e) = validate_resource_name(&found.name, "profile") {
            printer.status_simple(
                Role::Warn,
                format!(
                    "Skipping profile '{}': {}",
                    found.name.escape_default(),
                    cfgd_core::output::collapse_to_subject_line(&*e)
                ),
            );
            continue;
        }
        match config::load_profile(&found.path) {
            // The scan-entry name (filename stem / bundle dir) is what
            // `find_profile_path` resolves, so it is the name consumers can
            // act on; a divergent metadata.name would later fail NotFound.
            Ok(doc) => {
                if doc.metadata.name != found.name {
                    printer.status_simple(
                        Role::Warn,
                        format!(
                            "Profile file '{}' has metadata.name '{}'; using '{}'",
                            found.path.display(), // native-ok: human warn message, not a key
                            doc.metadata.name,
                            found.name
                        ),
                    );
                }
                names.push(found.name);
            }
            // Surface unparseable profiles instead of silently dropping them —
            // a missing profile in generated output is otherwise invisible.
            Err(e) => printer.status_simple(
                Role::Warn,
                format!(
                    "Skipping profile '{}': {}",
                    found.path.display(), // native-ok: human warn message, not a key
                    cfgd_core::output::collapse_to_subject_line(&e)
                ),
            ),
        }
    }
    names.sort();
    Ok(names)
}

/// Scan a modules/ directory and return sorted module names.
pub(in crate::cli) fn scan_module_names(
    modules_dir: &Path,
    printer: &Printer,
) -> anyhow::Result<Vec<String>> {
    let mut names = Vec::new();
    if modules_dir.exists() {
        for entry in std::fs::read_dir(modules_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir()
                && path.join("module.yaml").exists()
                && let Some(n) = entry.file_name().to_str()
            {
                // Same gate as scan_profile_names: raw stems end up inside
                // generated-workflow grep patterns and YAML matrix lines.
                if let Err(e) = validate_resource_name(n, "module") {
                    printer.status_simple(
                        Role::Warn,
                        format!(
                            "Skipping module '{}': {}",
                            n.escape_default(),
                            cfgd_core::output::collapse_to_subject_line(&*e)
                        ),
                    );
                    continue;
                }
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

/// The module cache directory honoring the `--cache-dir`/`CFGD_CACHE_DIR` override.
pub(in crate::cli) fn module_cache_dir(cli: &Cli) -> anyhow::Result<PathBuf> {
    module_cache_dir_for(cli.cache_dir.as_deref(), cli.scope())
}

/// Lower form for call sites that have the cache override but not the full `Cli`
/// (e.g. `cfgd init`, which threads the override through `InitArgs`).
pub(in crate::cli) fn module_cache_dir_for(
    cache_over: Option<&Path>,
    scope: cfgd_core::Scope,
) -> anyhow::Result<PathBuf> {
    Ok(cfgd_core::resolve_cache_dir(cache_over, scope)?.join("modules"))
}

/// Directory holding the apply mutex (`apply.lock`).
///
/// The apply mutex serializes the only operation that mutates live system
/// state, so it co-locates with the `state.db` it guards — the same dir the
/// daemon reconcile loop locks — and every acquirer must resolve it identically
/// (`--state-dir` flag > `CFGD_STATE_DIR` env > `XDG_STATE_HOME` > platform
/// default) regardless of how the process was launched, or the lock fails to
/// mutually-exclude and concurrent applies corrupt state.
pub(in crate::cli) fn apply_lock_dir(
    state_over: Option<&Path>,
    scope: cfgd_core::Scope,
) -> anyhow::Result<PathBuf> {
    cfgd_core::resolve_state_dir(state_over, scope)
        .map_err(|e| anyhow::anyhow!("cannot determine state directory: {}", e))
}

/// Resolve the effective config-file path honoring `--config` > `--config-dir` > default.
/// `config_is_explicit` is true when the user supplied `--config`/`CFGD_CONFIG`
/// (not the clap default). When the config arg is the default and a `config_dir`
/// override is present, the config file is `<config_dir>/<CONFIG_FILENAME>`.
pub fn effective_config_file(
    config_value: &Path,
    config_is_explicit: bool,
    config_dir: Option<&Path>,
) -> PathBuf {
    match (config_is_explicit, config_dir) {
        (false, Some(dir)) => dir.join(cfgd_core::config::CONFIG_FILENAME),
        _ => config_value.to_path_buf(),
    }
}

/// Build the no-config error so every command's missing-config path exits with
/// the same code (3) and names the path, matching plan/status/apply. Wraps the
/// typed `ConfigError::NotFound` with `CliErrorMeta` via `cli_error_ctx` so the
/// central sink renders one consistent payload while `main.rs` still downcasts
/// the inner `CfgdError` onto `ExitCode::NoConfig`. The returned error must be
/// propagated (`return Err(no_config_error(printer, path))`); it emits nothing.
pub(in crate::cli) fn no_config_error(_printer: &Printer, config_path: &Path) -> anyhow::Error {
    crate::cli::cli_error_ctx(
        cfgd_core::errors::CfgdError::Config(cfgd_core::errors::ConfigError::NotFound {
            path: config_path.to_path_buf(),
        })
        .into(),
        config_path.display().to_string(),
        "no_config",
        format!("config file not found: {}", config_path.display_posix()),
        serde_json::json!({ "path": cfgd_core::to_posix_string(config_path) }),
    )
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

/// Effective desired state every command resolves through.
///
/// `resolved` is the effective profile (local ⊕ sources), `modules` are resolved
/// against both the local module cache and source-delivered module roots, and
/// the two source maps carry per-source env (for template sandboxing) and commit
/// hashes (for apply provenance). Built by [`resolve_desired_state`].
pub(in crate::cli) struct DesiredState {
    pub resolved: ResolvedProfile,
    pub modules: Vec<cfgd_core::modules::ResolvedModule>,
    pub source_env: std::collections::HashMap<String, Vec<cfgd_core::config::EnvVar>>,
    pub source_commits: std::collections::HashMap<String, String>,
    /// Source security-constraint violations surfaced when the caller composed in
    /// [`ConstraintMode::Report`] (read paths). Empty for `Enforce` callers
    /// (apply/plan), which abort on the first violation instead.
    pub constraint_violations: Vec<cfgd_core::composition::ConstraintViolation>,
}

/// Compose the local profile with configured sources into an effective profile.
///
/// `refresh = true` fetches each source over the network (write paths:
/// `apply`/`plan`); `refresh = false` loads sources from their on-disk cache and
/// never touches the network (read paths). Delegates the actual merge to the
/// single composition code path in [`SourceManager::compose`], then displays and
/// persists any conflicts.
pub(in crate::cli) fn compose_with_sources(
    cli: &Cli,
    cfg: &config::CfgdConfig,
    local_resolved: &ResolvedProfile,
    printer: &Printer,
    refresh: bool,
    mode: composition::ConstraintMode,
) -> anyhow::Result<composition::CompositionResult> {
    if cfg.spec.sources.is_empty() {
        // No sources, return local profile as-is
        return Ok(composition::CompositionResult {
            resolved: local_resolved.clone(),
            conflicts: Vec::new(),
            source_env: std::collections::HashMap::new(),
            source_commits: std::collections::HashMap::new(),
            source_module_roots: Vec::new(),
            constraint_violations: Vec::new(),
        });
    }

    let cache_dir = source_cache_dir(cli)?;
    let mut mgr = SourceManager::new(&cache_dir);
    mgr.set_allow_unsigned(cfg.spec.security.as_ref().is_some_and(|s| s.allow_unsigned));
    if refresh {
        mgr.load_sources(&cfg.spec.sources, printer)?;
    } else {
        // Read paths stay offline: load from cache, warn+skip never-synced sources.
        mgr.load_sources_cached(&cfg.spec.sources, printer)?;
    }

    let result = mgr.compose(&cfg.spec.sources, local_resolved, mode)?;
    display_and_persist_conflicts(cli, &result, printer);

    // Surface the documented "scripts are shown in cfgd plan" promise: when a
    // subscriber opted in (`allowScripts: true`) to a source whose
    // `constraints.no_scripts` would otherwise block scripts, the script
    // execution must be visible. Non-fatal — the opt-in already permitted it.
    for spec in &cfg.spec.sources {
        if spec.subscription.allow_scripts
            && let Some(cached) = mgr.get(&spec.name)
            && cached.manifest.spec.policy.constraints.no_scripts
        {
            printer.note(format!(
                "source '{}' scripts will run because allowScripts is set (constraints.no_scripts is overridden by your subscription)",
                spec.name
            ));
        }
    }

    Ok(result)
}

/// Render composition conflicts under a section and persist them to the state
/// store for `status`/history. Best-effort persistence: a state error is logged,
/// not fatal, so a read-only filesystem never blocks a compose.
fn display_and_persist_conflicts(
    cli: &Cli,
    result: &composition::CompositionResult,
    printer: &Printer,
) {
    if result.conflicts.is_empty() {
        return;
    }
    let guard = printer.section("Source Conflicts");
    for conflict in &result.conflicts {
        match conflict.resolution_type {
            composition::ResolutionType::Locked => {
                guard.status_simple(Role::Warn, &conflict.details);
            }
            composition::ResolutionType::Required
            | composition::ResolutionType::Rejected
            | composition::ResolutionType::Override => {
                guard.status_simple(Role::Info, &conflict.details);
            }
            composition::ResolutionType::Default => {}
        }
    }
    drop(guard);

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

/// The single desired-state resolver every command flows through.
///
/// Composes the local profile with configured sources (network fetch when
/// `refresh = true`, cache-only otherwise), then resolves the effective
/// module set against both the local module cache and the source-delivered
/// module roots. With no sources configured this collapses to resolving the
/// local profile's own modules with empty source maps — identical to the old
/// per-command path, so the no-sources case is a pure regression.
///
/// `module_filter` scopes module resolution to a single module (apply/diff
/// `--module`); `None` resolves the whole effective profile.
///
/// Errors from `compose` (constraint violations, malformed cached manifest,
/// failed signature) propagate so an invalid source config fails every command
/// consistently — a command that reports state must not silently report empty
/// when the desired state is broken.
pub(in crate::cli) fn resolve_desired_state(
    cli: &Cli,
    cfg: &config::CfgdConfig,
    local_resolved: &ResolvedProfile,
    module_filter: Option<&str>,
    printer: &Printer,
    refresh: bool,
    mode: composition::ConstraintMode,
) -> anyhow::Result<DesiredState> {
    let composition = compose_with_sources(cli, cfg, local_resolved, printer, refresh, mode)?;
    let composition::CompositionResult {
        resolved,
        source_env,
        source_commits,
        source_module_roots,
        constraint_violations,
        ..
    } = composition;

    let config_dir = config_dir(cli);
    let module_names = match module_filter {
        Some(name) => vec![name.to_string()],
        None => resolved.merged.modules.clone(),
    };

    let modules = if module_names.is_empty() {
        Vec::new()
    } else {
        // Config-aware registry so a module that references a custom package
        // manager (declared in cfg / composed packages) resolves identically on
        // every command — matching the apply path's registry.
        let mut registry =
            build_registry_with_config_and_packages(Some(cfg), Some(&resolved.merged.packages));
        registry
            .package_managers
            .extend(packages::custom_managers(&resolved.merged.packages.custom));
        let platform = Platform::detect();
        let mgr_map = managers_map(&registry);
        let cache_base = module_cache_dir(cli)?;
        match modules::resolve_modules(
            &module_names,
            &config_dir,
            &cache_base,
            &source_module_roots,
            &platform,
            &mgr_map,
            printer,
        ) {
            Ok(mods) => mods,
            // A `--module` filter naming a module that does not resolve degrades
            // to empty (the command reports "module not found"), matching apply's
            // module-filter behavior. A full-profile resolution error propagates.
            Err(e) if module_filter.is_some() => {
                tracing::debug!("module filter '{}' not found: {}", module_names[0], e);
                Vec::new()
            }
            Err(e) => return Err(e.into()),
        }
    };

    Ok(DesiredState {
        resolved,
        modules,
        source_env,
        source_commits,
        constraint_violations,
    })
}

/// Outcome of the shared cosign sign + SLSA-attest tail.
#[derive(Debug)]
pub(in crate::cli) struct SignAttestOutcome {
    pub signed: bool,
    pub attested: bool,
}

/// Cosign-sign and/or attach SLSA provenance to an already-pushed OCI artifact.
///
/// Shared by `cfgd module push` and `cfgd image pack`: both push an artifact,
/// then optionally sign it and attach provenance derived from the local git
/// `origin`/`HEAD`. Errors route through `collapse_to_subject_line` so a
/// multi-line cosign stderr can't trip the renderer's single-line invariant.
pub(in crate::cli) fn sign_and_attest(
    printer: &Printer,
    artifact: &str,
    digest: &str,
    key: Option<&str>,
    sign: bool,
    attest: bool,
) -> anyhow::Result<SignAttestOutcome> {
    if sign {
        cfgd_core::oci::sign_artifact(artifact, key).map_err(|e| {
            cli_error(
                artifact,
                "sign_failed",
                cfgd_core::output::collapse_to_subject_line(&e),
                serde_json::json!({ "artifact": artifact }),
            )
        })?;
        printer.status_simple(Role::Ok, "Signed artifact with cosign");
    }

    let mut attested = false;
    if attest {
        let repo = cfgd_core::detect_git_remote();
        let commit = cfgd_core::detect_git_head();
        if repo.is_none() || commit.is_none() {
            printer.status_simple(
                Role::Warn,
                "No git remote/HEAD detected — SLSA provenance will record source as \"unknown\"",
            );
        }
        let repo = repo.unwrap_or_else(|| "unknown".to_string());
        let commit = commit.unwrap_or_else(|| "unknown".to_string());

        let provenance = cfgd_core::oci::generate_slsa_provenance(&repo, &commit).map_err(|e| {
            cli_error(
                artifact,
                "attest_failed",
                cfgd_core::output::collapse_to_subject_line(&e),
                serde_json::json!({ "artifact": artifact, "digest": digest, "step": "provenance" }),
            )
        })?;
        // Write the predicate into a fresh temp DIR rather than a NamedTempFile:
        // atomic_write_str renames a sibling over the target, and on Windows you
        // cannot replace a file that still has an open handle (NamedTempFile keeps
        // one) → ERROR_ACCESS_DENIED. A dir-joined path carries no open handle.
        let pred_dir = tempfile::tempdir()?;
        let pred_path = pred_dir.path().join("provenance.json");
        cfgd_core::atomic_write_str(&pred_path, &provenance)?;
        cfgd_core::oci::attach_attestation(
            artifact,
            // native-ok: local predicate path for the co-located cosign subprocess
            &pred_path.display().to_string(),
            key,
        )
        .map_err(|e| {
            cli_error(
                artifact,
                "attest_failed",
                cfgd_core::output::collapse_to_subject_line(&e),
                serde_json::json!({ "artifact": artifact, "step": "attach" }),
            )
        })?;
        // pred_dir must outlive attach_attestation so the subprocess can read it.
        drop(pred_dir);
        printer.status_simple(Role::Ok, "Attached SLSA provenance attestation");
        attested = true;
    }

    Ok(SignAttestOutcome {
        signed: sign,
        attested,
    })
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
            config_explicit: false,
            profile: None,
            verbose: 0,
            quiet: true,
            no_color: true,
            output: OutputFormatArg(OutputFormat::Table),
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
    // module_cache_dir / effective_config_file
    // ---------------------------------------------------------------------------

    #[test]
    fn module_cache_dir_for_appends_modules_to_override() {
        let over = PathBuf::from("/over/cache");
        // Explicit override wins regardless of scope; use the per-user default.
        let dir = module_cache_dir_for(Some(&over), cfgd_core::Scope::User).unwrap();
        assert_eq!(dir, over.join("modules"));
    }

    #[test]
    fn module_cache_dir_honors_cache_override() {
        let mut cli = make_cli(PathBuf::from("cfgd.yaml"));
        cli.cache_dir = Some(PathBuf::from("/over/cache"));
        let dir = module_cache_dir(&cli).unwrap();
        assert_eq!(dir, PathBuf::from("/over/cache").join("modules"));
    }

    #[test]
    fn effective_config_file_explicit_config_wins_over_dir() {
        let cfg = Path::new("/explicit/my.yaml");
        let dir = PathBuf::from("/some/config-dir");
        let out = effective_config_file(cfg, true, Some(&dir));
        assert_eq!(out, cfg);
    }

    #[test]
    fn effective_config_file_default_config_uses_config_dir() {
        let cfg = Path::new("/default/cfgd.yaml");
        let dir = PathBuf::from("/some/config-dir");
        let out = effective_config_file(cfg, false, Some(&dir));
        assert_eq!(out, dir.join(cfgd_core::config::CONFIG_FILENAME));
    }

    #[test]
    fn effective_config_file_default_config_no_dir_is_unchanged() {
        let cfg = Path::new("/default/cfgd.yaml");
        let out = effective_config_file(cfg, false, None);
        assert_eq!(out, cfg);
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
    // sources_providing_profile
    // ---------------------------------------------------------------------------

    /// Build a config subscribing to one source named `test-src`, plus a cache
    /// layout rooted at `<tmp>/cache` for that source. Returns (cli, cfg); the
    /// caller populates `<tmp>/cache/sources/test-src/profiles` in the form
    /// under test.
    fn cli_and_cfg_with_source_cache(tmp: &std::path::Path) -> (Cli, CfgdConfig) {
        let config_yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n  sources:\n    - name: test-src\n      origin:\n        type: Git\n        url: https://example.com/config.git\n        branch: master\n";
        let config_path = tmp.join("cfgd.yaml");
        std::fs::write(&config_path, config_yaml).unwrap();
        let cfg = config::load_config(&config_path).unwrap();
        let mut cli = make_cli(config_path);
        cli.cache_dir = Some(tmp.join("cache"));
        (cli, cfg)
    }

    #[test]
    fn sources_providing_profile_sees_legacy_flat_form() {
        let tmp = tempdir().unwrap();
        let (cli, cfg) = cli_and_cfg_with_source_cache(tmp.path());
        let profiles = tmp.path().join("cache/sources/test-src/profiles");
        std::fs::create_dir_all(&profiles).unwrap();
        std::fs::write(profiles.join("team.yaml"), PROFILE_YAML).unwrap();

        assert_eq!(
            sources_providing_profile(&cli, &cfg, "team"),
            vec!["test-src"]
        );
    }

    #[test]
    fn sources_providing_profile_sees_canonical_bundle_form() {
        let tmp = tempdir().unwrap();
        let (cli, cfg) = cli_and_cfg_with_source_cache(tmp.path());
        let bundle = tmp.path().join("cache/sources/test-src/profiles/team");
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::write(bundle.join("profile.yaml"), PROFILE_YAML).unwrap();

        assert_eq!(
            sources_providing_profile(&cli, &cfg, "team"),
            vec!["test-src"]
        );
    }

    #[test]
    fn sources_providing_profile_ambiguous_name_still_counts() {
        let tmp = tempdir().unwrap();
        let (cli, cfg) = cli_and_cfg_with_source_cache(tmp.path());
        // BOTH forms on disk: an ambiguous layout must still count — the
        // source does deliver the profile, however malformed the cache.
        let profiles = tmp.path().join("cache/sources/test-src/profiles");
        let bundle = profiles.join("team");
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::write(bundle.join("profile.yaml"), PROFILE_YAML).unwrap();
        std::fs::write(profiles.join("team.yaml"), PROFILE_YAML).unwrap();

        assert_eq!(
            sources_providing_profile(&cli, &cfg, "team"),
            vec!["test-src"]
        );
    }

    #[test]
    fn sources_providing_profile_no_match_returns_empty() {
        let tmp = tempdir().unwrap();
        let (cli, cfg) = cli_and_cfg_with_source_cache(tmp.path());
        let profiles = tmp.path().join("cache/sources/test-src/profiles");
        std::fs::create_dir_all(&profiles).unwrap();
        std::fs::write(profiles.join("other.yaml"), PROFILE_YAML).unwrap();

        assert!(sources_providing_profile(&cli, &cfg, "team").is_empty());
    }

    // ---------------------------------------------------------------------------
    // scan_module_names
    // ---------------------------------------------------------------------------

    #[test]
    fn scan_module_names_empty_dir_returns_empty() {
        let tmp = tempdir().unwrap();
        let (printer, _buf) =
            cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
        let names = scan_module_names(tmp.path(), &printer).unwrap();
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
        let (printer, _buf) =
            cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
        let names = scan_module_names(tmp.path(), &printer).unwrap();
        assert_eq!(names, vec!["alpha", "beta"]);
    }

    #[test]
    fn scan_module_names_missing_dir_returns_empty() {
        let tmp = tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist");
        let (printer, _buf) =
            cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
        let names = scan_module_names(&missing, &printer).unwrap();
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

        let result = compose_with_sources(
            &cli,
            &cfg,
            &local,
            &printer,
            true,
            composition::ConstraintMode::Enforce,
        )
        .unwrap();

        // No sources → resolved must equal the local profile we passed in.
        assert_eq!(result.resolved.merged.modules, local.merged.modules);
        assert!(result.conflicts.is_empty());
        assert!(result.source_env.is_empty());
        assert!(result.source_commits.is_empty());
    }

    /// Build a minimal local git repo that acts as a cfgd source.
    ///
    /// The source's `<profile_name>.yaml` profile declares a module named
    /// `source-module` AND a `cargo` package `source-pkg`, and the source ships a
    /// body for `source-module` (in `modules/`, allow-listed via
    /// `provides.modules`). This lets composition + module resolution be asserted
    /// on a non-empty contribution of BOTH a package and a module from the source.
    fn create_local_source_repo(root: &std::path::Path, profile_name: &str) -> PathBuf {
        create_local_source_repo_with_form(root, profile_name, false)
    }

    /// Like [`create_local_source_repo`], but `canonical` selects the profile
    /// manifest layout inside the repo: `profiles/<name>/profile.yaml` (true)
    /// vs the legacy flat `profiles/<name>.yaml` (false).
    fn create_local_source_repo_with_form(
        root: &std::path::Path,
        profile_name: &str,
        canonical: bool,
    ) -> PathBuf {
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
            "apiVersion: cfgd.io/v1alpha1\nkind: ConfigSource\nmetadata:\n  name: test-src\nspec:\n  provides:\n    profiles:\n      - {profile_name}\n    modules:\n      - source-module\n"
        );
        std::fs::write(repo_dir.join("cfgd-source.yaml"), &manifest).unwrap();

        let profile_yaml = format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: {profile_name}\nspec:\n  modules:\n    - source-module\n  packages:\n    cargo:\n      - source-pkg\n"
        );
        let profile_path = if canonical {
            cfgd_core::config::canonical_profile_path(&repo_dir.join("profiles"), profile_name)
        } else {
            repo_dir
                .join("profiles")
                .join(format!("{profile_name}.yaml"))
        };
        std::fs::create_dir_all(profile_path.parent().unwrap()).unwrap();
        std::fs::write(&profile_path, &profile_yaml).unwrap();

        // Source-delivered module body, allow-listed by the manifest above.
        let module_dir = repo_dir.join("modules").join("source-module");
        std::fs::create_dir_all(&module_dir).unwrap();
        std::fs::write(
            module_dir.join("module.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: source-module\nspec:\n  packages:\n    - name: module-pkg\n      prefer: [cargo]\n",
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

    /// Build a cfgd.yaml that subscribes to a single local source selecting
    /// `profile`, plus a local `default.yaml` profile, under `tmp`. Returns the
    /// config path. Mirrors the layout the existing source tests build.
    fn write_config_with_local_source(
        tmp: &std::path::Path,
        source_repo: &std::path::Path,
        source_profile: &str,
    ) -> PathBuf {
        let config_yaml = format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n  sources:\n    - name: test-src\n      origin:\n        type: Git\n        url: {}\n        branch: master\n      subscription:\n        profile: {}\n",
            source_repo.display(),
            source_profile,
        );
        let config_path = tmp.join("cfgd.yaml");
        std::fs::write(&config_path, &config_yaml).unwrap();
        let profiles_dir = tmp.join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(profiles_dir.join("default.yaml"), PROFILE_YAML).unwrap();
        config_path
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
        cli.cache_dir = Some(tmp.path().join("cache"));

        let cfg = config::load_config(&config_path).unwrap();
        let local = empty_resolved_profile("my-module");
        let printer = quiet_printer();

        let result = compose_with_sources(
            &cli,
            &cfg,
            &local,
            &printer,
            true,
            composition::ConstraintMode::Enforce,
        )
        .unwrap();

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

    /// Same end-to-end composition, but the source repo carries its profile in
    /// the canonical bundle form (profiles/team/profile.yaml). The clone →
    /// subscribe → compose path must resolve it identically to the flat form.
    #[test]
    #[serial]
    fn compose_with_sources_merges_canonical_form_source_profile() {
        let tmp = tempdir().unwrap();
        let source_repo = create_local_source_repo_with_form(tmp.path(), "team", true);

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
        cli.cache_dir = Some(tmp.path().join("cache"));

        let cfg = config::load_config(&config_path).unwrap();
        let local = empty_resolved_profile("my-module");
        let printer = quiet_printer();

        let result = compose_with_sources(
            &cli,
            &cfg,
            &local,
            &printer,
            true,
            composition::ConstraintMode::Enforce,
        )
        .unwrap();

        assert!(
            result.source_commits.contains_key("test-src"),
            "expected source_commits to record 'test-src', got: {:?}",
            result.source_commits
        );
        assert!(
            result
                .resolved
                .merged
                .modules
                .contains(&"source-module".to_string()),
            "canonical-form source profile not merged: {:?}",
            result.resolved.merged.modules
        );
    }

    // ---------------------------------------------------------------------------
    // resolve_desired_state — the one resolver every command flows through
    // ---------------------------------------------------------------------------

    /// A consumer subscribed to a source whose profile contributes a PACKAGE and
    /// a MODULE: the read path (`refresh = false`) sees both the source package
    /// and the source-delivered module body as desired state. This is the
    /// coherence fix — before it, read paths resolved the local-only profile with
    /// no source roots, so this would be empty.
    #[test]
    #[serial]
    fn resolve_desired_state_read_path_sees_source_package_and_module() {
        let tmp = tempdir().unwrap();
        let source_repo = create_local_source_repo(tmp.path(), "team");
        let config_path = write_config_with_local_source(tmp.path(), &source_repo, "team");

        let _allow = EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
        let mut cli = make_cli(config_path.clone());
        cli.state_dir = Some(tmp.path().join("state"));
        cli.cache_dir = Some(tmp.path().join("cache"));
        let cfg = config::load_config(&config_path).unwrap();
        let local = empty_resolved_profile("my-module");
        let printer = quiet_printer();

        // Prime the cache with a refresh so the cache-only read path has a cache
        // dir to read (the daemon's sync task plays this role in production).
        compose_with_sources(
            &cli,
            &cfg,
            &local,
            &printer,
            true,
            composition::ConstraintMode::Enforce,
        )
        .unwrap();

        // Read path: cache-only, no network.
        let desired = resolve_desired_state(
            &cli,
            &cfg,
            &local,
            None,
            &printer,
            false,
            composition::ConstraintMode::Enforce,
        )
        .unwrap();

        // The source profile's cargo package must be in the effective desired state.
        let cargo_pkgs: Vec<String> = desired
            .resolved
            .merged
            .packages
            .cargo
            .as_ref()
            .map(|c| c.packages.clone())
            .unwrap_or_default();
        assert!(
            cargo_pkgs.iter().any(|p| p == "source-pkg"),
            "read path missing source package: {cargo_pkgs:?}"
        );

        // The source-delivered module body must resolve (origin tagged to the source).
        let sm = desired
            .modules
            .iter()
            .find(|m| m.name == "source-module")
            .expect("read path must resolve source-delivered module body");
        assert_eq!(
            sm.origin.as_deref(),
            Some("test-src"),
            "source-delivered module must be tagged with its source origin"
        );
    }

    /// Cache-miss on a read path (source configured but never synced) → warn +
    /// skip; the command still succeeds with local-only state.
    #[test]
    #[serial]
    fn resolve_desired_state_read_path_cache_miss_falls_back_to_local() {
        let tmp = tempdir().unwrap();
        let source_repo = create_local_source_repo(tmp.path(), "team");
        let config_path = write_config_with_local_source(tmp.path(), &source_repo, "team");

        let _allow = EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
        let mut cli = make_cli(config_path.clone());
        // Point the source cache at a fresh, empty dir so the source is "never
        // synced" — no refresh primes it.
        cli.state_dir = Some(tmp.path().join("never-synced-state"));
        cli.cache_dir = Some(tmp.path().join("never-synced-cache"));
        let cfg = config::load_config(&config_path).unwrap();
        // Local profile carries a local package but no modules, so module
        // resolution is trivially empty and the assertion focuses on the
        // cache-miss fallback (source contribution absent, local survives).
        let mut local = ResolvedProfile {
            layers: Vec::new(),
            merged: MergedProfile::default(),
        };
        local.merged.packages.cargo = Some(cfgd_core::config::CargoSpec {
            file: None,
            packages: vec!["local-pkg".to_string()],
        });
        let printer = quiet_printer();

        // No prime: cache dir for 'test-src' does not exist.
        let desired = resolve_desired_state(
            &cli,
            &cfg,
            &local,
            None,
            &printer,
            false,
            composition::ConstraintMode::Enforce,
        )
        .unwrap();

        // Falls back to local: source package absent, local package survives.
        let cargo_pkgs: Vec<String> = desired
            .resolved
            .merged
            .packages
            .cargo
            .as_ref()
            .map(|c| c.packages.clone())
            .unwrap_or_default();
        assert!(
            !cargo_pkgs.iter().any(|p| p == "source-pkg"),
            "cache-miss must NOT include source package: {cargo_pkgs:?}"
        );
        assert!(
            cargo_pkgs.iter().any(|p| p == "local-pkg"),
            "local package must survive cache-miss fallback: {cargo_pkgs:?}"
        );
        assert!(
            desired.modules.is_empty(),
            "no local modules → empty module set on cache-miss fallback"
        );
    }

    /// The coherence invariant: apply (`refresh = true`) and a read path
    /// (`refresh = false`) compute the SAME effective module set for the same
    /// config + primed cache.
    #[test]
    #[serial]
    fn resolve_desired_state_apply_and_read_compute_same_module_set() {
        let tmp = tempdir().unwrap();
        let source_repo = create_local_source_repo(tmp.path(), "team");
        let config_path = write_config_with_local_source(tmp.path(), &source_repo, "team");

        let _allow = EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
        let mut cli = make_cli(config_path.clone());
        cli.state_dir = Some(tmp.path().join("state"));
        cli.cache_dir = Some(tmp.path().join("cache"));
        let cfg = config::load_config(&config_path).unwrap();
        let local = empty_resolved_profile("my-module");
        let printer = quiet_printer();

        // refresh = true (apply/plan path) primes the cache AND resolves.
        let apply_side = resolve_desired_state(
            &cli,
            &cfg,
            &local,
            None,
            &printer,
            true,
            composition::ConstraintMode::Enforce,
        )
        .unwrap();
        // refresh = false (read path) on the now-primed cache.
        let read_side = resolve_desired_state(
            &cli,
            &cfg,
            &local,
            None,
            &printer,
            false,
            composition::ConstraintMode::Enforce,
        )
        .unwrap();

        let mut apply_modules: Vec<String> =
            apply_side.modules.iter().map(|m| m.name.clone()).collect();
        let mut read_modules: Vec<String> =
            read_side.modules.iter().map(|m| m.name.clone()).collect();
        apply_modules.sort();
        read_modules.sort();
        assert_eq!(
            apply_modules, read_modules,
            "apply and read paths must compute an identical effective module set"
        );
        assert!(
            apply_modules.contains(&"source-module".to_string()),
            "expected source-module in the shared effective set: {apply_modules:?}"
        );
    }

    /// No-sources regression: with no sources configured, `resolve_desired_state`
    /// collapses to resolving the local profile's own modules with empty source
    /// maps — identical to the old per-command path.
    #[test]
    #[serial]
    fn resolve_desired_state_no_sources_resolves_local_only() {
        let tmp = tempdir().unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(&config_path, CONFIG_YAML).unwrap();
        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(profiles_dir.join("default.yaml"), PROFILE_YAML).unwrap();

        let cli = make_cli(config_path.clone());
        let cfg = config::load_config(&config_path).unwrap();
        // Local profile declares no modules → empty module set, empty source maps.
        let local = ResolvedProfile {
            layers: Vec::new(),
            merged: MergedProfile::default(),
        };
        let printer = quiet_printer();

        let desired = resolve_desired_state(
            &cli,
            &cfg,
            &local,
            None,
            &printer,
            false,
            composition::ConstraintMode::Enforce,
        )
        .unwrap();
        assert!(desired.modules.is_empty());
        assert!(desired.source_env.is_empty());
        assert!(desired.source_commits.is_empty());
        assert_eq!(
            desired.resolved.merged.modules, local.merged.modules,
            "no-sources resolved must equal the local profile"
        );
    }

    // ---------------------------------------------------------------------------
    // sign_and_attest
    // ---------------------------------------------------------------------------

    #[test]
    fn sign_and_attest_no_op_returns_both_false_without_cosign() {
        // Neither flag set: the function must not touch cosign at all and must
        // report nothing signed or attested. No shim is installed, so any
        // cosign shell-out would fail — proving the no-op path is taken.
        let printer = quiet_printer();
        let outcome = sign_and_attest(
            &printer,
            "localhost:5000/x:v1",
            "sha256:dead",
            None,
            false,
            false,
        )
        .expect("no-op sign/attest must succeed");
        assert!(!outcome.signed, "signed must be false when sign=false");
        assert!(
            !outcome.attested,
            "attested must be false when attest=false"
        );
    }

    #[test]
    #[serial]
    fn sign_and_attest_attest_without_git_warns_and_records_unknown_source() {
        // Attesting from a directory with no git remote/HEAD must emit the
        // "source as unknown" warning and still complete the attestation.
        let shim = cfgd_core::test_helpers::CosignTestShim::builder()
            .with_argv_logging(false)
            .with_exit(0)
            .install();
        let dir = tempdir().expect("tempdir");
        let _cwd = cfgd_core::test_helpers::CwdGuard::set(dir.path()).expect("cwd guard");

        // Normal verbosity: the Warn role is suppressed under Quiet.
        let (printer, cap) = Printer::for_test_at(Verbosity::Normal);
        let outcome = sign_and_attest(
            &printer,
            "localhost:5000/x:v1",
            "sha256:dead",
            None,
            false,
            true,
        )
        .expect("attest must succeed under the cosign shim");
        drop(printer);
        drop(shim);

        assert!(!outcome.signed, "signed must be false when sign=false");
        assert!(
            outcome.attested,
            "attested must be true after a successful attach"
        );
        let out = cap.lock().expect("capture lock");
        assert!(
            out.contains("record source as"),
            "no-git attestation must warn about unknown provenance source, got: {out}"
        );
    }

    #[test]
    #[serial]
    fn sign_and_attest_sign_failure_maps_to_sign_failed_meta() {
        // A non-zero cosign exit on the sign step must surface as a
        // CliErrorMeta with error_kind "sign_failed".
        let shim = cfgd_core::test_helpers::CosignTestShim::builder()
            .with_argv_logging(false)
            .with_exit(1)
            .install();
        let printer = quiet_printer();
        let err = sign_and_attest(
            &printer,
            "localhost:5000/x:v1",
            "sha256:dead",
            None,
            true,
            false,
        )
        .expect_err("failing cosign sign must return Err");
        drop(shim);

        let meta = err
            .downcast_ref::<crate::cli::CliErrorMeta>()
            .expect("sign failure returns CliErrorMeta");
        assert_eq!(
            meta.error_kind, "sign_failed",
            "cosign sign failure must map to sign_failed: {meta:?}"
        );
    }

    #[test]
    #[serial]
    fn sign_and_attest_attest_failure_maps_to_attest_failed_meta() {
        // Provenance generation succeeds, but a non-zero cosign exit on the
        // attach step must surface as error_kind "attest_failed".
        let shim = cfgd_core::test_helpers::CosignTestShim::builder()
            .with_argv_logging(false)
            .with_exit(1)
            .install();
        let dir = tempdir().expect("tempdir");
        let _cwd = cfgd_core::test_helpers::CwdGuard::set(dir.path()).expect("cwd guard");

        let printer = quiet_printer();
        let err = sign_and_attest(
            &printer,
            "localhost:5000/x:v1",
            "sha256:dead",
            None,
            false,
            true,
        )
        .expect_err("failing cosign attest must return Err");
        drop(shim);

        let meta = err
            .downcast_ref::<crate::cli::CliErrorMeta>()
            .expect("attest failure returns CliErrorMeta");
        assert_eq!(
            meta.error_kind, "attest_failed",
            "cosign attach failure must map to attest_failed: {meta:?}"
        );
    }

    // ---------------------------------------------------------------------------
    // display_and_persist_conflicts
    // ---------------------------------------------------------------------------

    #[test]
    #[serial]
    fn display_and_persist_conflicts_routes_roles_and_persists() {
        use std::collections::HashMap;

        let tmp = tempdir().expect("tempdir");
        let mut cli = make_cli(tmp.path().join("cfgd.yaml"));
        cli.state_dir = Some(tmp.path().join("state"));

        let result = composition::CompositionResult {
            resolved: ResolvedProfile {
                layers: Vec::new(),
                merged: MergedProfile::default(),
            },
            conflicts: vec![
                composition::ConflictResolution {
                    resource_id: "pkg.ripgrep".to_string(),
                    resolution_type: composition::ResolutionType::Locked,
                    winning_source: "base".to_string(),
                    details: "locked by base".to_string(),
                },
                composition::ConflictResolution {
                    resource_id: "pkg.fd".to_string(),
                    resolution_type: composition::ResolutionType::Override,
                    winning_source: "team".to_string(),
                    details: "overridden by team".to_string(),
                },
                composition::ConflictResolution {
                    resource_id: "pkg.bat".to_string(),
                    resolution_type: composition::ResolutionType::Default,
                    winning_source: "base".to_string(),
                    details: "silent default".to_string(),
                },
            ],
            source_env: HashMap::new(),
            source_commits: HashMap::new(),
            source_module_roots: Vec::new(),
            constraint_violations: Vec::new(),
        };

        let (printer, cap) = Printer::for_test_at(Verbosity::Normal);
        display_and_persist_conflicts(&cli, &result, &printer);
        drop(printer);

        let out = cap.lock().expect("capture lock");
        assert!(
            out.contains("Source Conflicts"),
            "conflicts must render under their section: {out}"
        );
        // Locked routes to a Warn line, Override to an Info line; both details
        // must surface. The Default resolution is intentionally silent.
        assert!(
            out.contains("locked by base"),
            "Locked conflict detail must render: {out}"
        );
        assert!(
            out.contains("overridden by team"),
            "Override conflict detail must render: {out}"
        );
        assert!(
            !out.contains("silent default"),
            "Default resolution must NOT render a line: {out}"
        );

        // Persistence ran against a real state store (temp state_dir); reopening
        // it must succeed, proving the open_state_store branch was exercised.
        assert!(
            open_state_store(cli.state_dir.as_deref()).is_ok(),
            "state store must be openable after persistence"
        );
    }
}
