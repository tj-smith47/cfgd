use super::*;
use cfgd_core::PathDisplayExt;
use cfgd_core::output::{Printer, Role};

/// Write a freshly scaffolded manifest: prepend the editor schema modeline and
/// write atomically.
///
/// Lives in the binary crate on purpose — the modeline's schema version comes
/// from `env!("CARGO_PKG_VERSION")` evaluated HERE, so it is always the cfgd
/// binary's version (the one the vendored SchemaStore schemas are published
/// under), never cfgd-core's independently-versioned one (which would 404).
/// Scaffold-only: rewrite paths of user-owned files must never inject a
/// modeline and must not use this.
pub(in crate::cli) fn write_scaffold(
    kind: cfgd_core::config::SchemaDocKind,
    path: &Path,
    body: &str,
) -> anyhow::Result<()> {
    let content = cfgd_core::config::with_schema_modeline(kind, env!("CARGO_PKG_VERSION"), body);
    cfgd_core::atomic_write_str(path, &content)?;
    Ok(())
}

/// Rewrite a user-owned YAML document, re-prepending the file's existing
/// leading comment block (banner comments and the schema modeline).
///
/// Counterpart to `write_scaffold`: scaffolds inject a modeline; rewrites only
/// preserve what the file already had — never inject. Mid-document comments
/// cannot survive the serde round-trip and remain lost.
pub(in crate::cli) fn rewrite_user_yaml<T: serde::Serialize>(
    path: &Path,
    value: &T,
) -> anyhow::Result<()> {
    // A missing original is a legitimate first write (comment-free); any
    // other read failure on an EXISTING file must abort the rewrite —
    // atomic_write_str renames over the target regardless of its
    // readability, which would silently strip the comments this helper
    // exists to preserve.
    let original = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    rewrite_user_yaml_with_original(path, &original, value)
}

/// [`rewrite_user_yaml`] for callers that already hold the file's pre-read
/// content, avoiding a second read of the same file.
pub(in crate::cli) fn rewrite_user_yaml_with_original<T: serde::Serialize>(
    path: &Path,
    original: &str,
    value: &T,
) -> anyhow::Result<()> {
    let yaml = serde_yaml::to_string(value)?;
    cfgd_core::atomic_write_str(
        path,
        &cfgd_core::config::with_leading_comments(original, &yaml),
    )?;
    Ok(())
}

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
            // Membership probe of the three known manifest paths — a full
            // directory scan per source would stat every profile just to
            // answer "is this one name present?".
            let dir = mgr.cached_profiles_dir(&spec.name);
            matches!(
                cfgd_core::config::find_profile_path(&dir, profile_name),
                Ok(_) | Err(cfgd_core::errors::ConfigError::AmbiguousProfile { .. })
            )
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
                "workflow regeneration failed ({}); the on-disk workflow is stale until this is resolved and the workflow is regenerated",
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
mod tests;
