use super::*;
use cfgd_core::PathDisplayExt;
use cfgd_core::output::{Printer, Role};

/// An env var or alias as the `name="value"` cfgd renders it.
///
/// The value is quoted unconditionally, through the same
/// [`cfgd_core::posix_double_quoted`] the generated env file's `export EDITOR="nvim"`
/// and `alias catn="cat -n"` lines are written with, so the line that confirms
/// a write and the file it wrote spell one assignment one way. Unquoted, a
/// value holding a space is a different value to the eye: `catn=cat -n` reads
/// as the alias `cat` with a stray `-n` beside it, which is exactly the
/// ambiguity the user's own `--alias catn='cat -n'` quoting existed to remove.
/// Conditional quoting would trade that for a shape the reader has to decode
/// before knowing which rule produced it.
pub(in crate::cli) fn quoted_assignment(name: &str, value: &str) -> String {
    format!("{name}={}", cfgd_core::posix_double_quoted(value))
}

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
pub(in crate::cli) fn rewrite_user_yaml<T: serde::Serialize + serde::de::DeserializeOwned>(
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
pub(in crate::cli) fn rewrite_user_yaml_with_original<
    T: serde::Serialize + serde::de::DeserializeOwned,
>(
    path: &Path,
    original: &str,
    value: &T,
) -> anyhow::Result<()> {
    let mut tree = serde_yaml::to_value(value)?;
    prune_absent_sections(&mut tree, 0);
    // An original that does not parse declared nothing; every default is then
    // undeclared, and dropping one changes nothing a reader gets back.
    let declared = serde_yaml::from_str(original).unwrap_or(serde_yaml::Value::Null);
    prune_undeclared_defaults::<T>(&mut tree, &declared);
    let yaml = serde_yaml::to_string(&tree)?;
    cfgd_core::atomic_write_str(
        path,
        &cfgd_core::config::with_leading_comments(original, &yaml),
    )?;
    Ok(())
}

/// Drop every mapping entry whose value is `null`, `[]` or `{}` — what a
/// typed config's `None` and empty-collection fields serialize as. A serde
/// round-trip of a user's `cfgd.yaml` otherwise writes `daemon: null`,
/// `origin: []`, `secrets: null`, … for every section they never declared,
/// and a `daemon: null` is also what `config set daemon.…` used to refuse to
/// traverse. Pruning here, at the one write every doc rewrite funnels through,
/// covers every doc type and every field added later without a per-field
/// `skip_serializing_if`. Deserialization is unaffected: every one of those
/// fields is `#[serde(default)]`, so an absent key reads back as the same
/// `None`/empty value that was written. The document's own top-level keys
/// (`spec`, `metadata`) are never dropped, and a sequence element is data and
/// is never dropped either.
fn prune_absent_sections(value: &mut serde_yaml::Value, depth: usize) {
    match value {
        serde_yaml::Value::Mapping(map) => {
            for entry in map.values_mut() {
                prune_absent_sections(entry, depth + 1);
            }
            if depth > 0 {
                map.retain(|_, v| !is_absent_section(v));
            }
        }
        serde_yaml::Value::Sequence(seq) => {
            for entry in seq.iter_mut() {
                prune_absent_sections(entry, depth + 1);
            }
        }
        _ => {}
    }
}

fn is_absent_section(value: &serde_yaml::Value) -> bool {
    match value {
        serde_yaml::Value::Null => true,
        serde_yaml::Value::Sequence(seq) => seq.is_empty(),
        serde_yaml::Value::Mapping(map) => map.is_empty(),
        _ => false,
    }
}

/// One step of a path into a YAML tree: a mapping key or a sequence index.
#[derive(Clone)]
enum YamlStep {
    Key(serde_yaml::Value),
    Index(usize),
}

/// Drop every scalar the author never declared whose value is the field's own
/// default (`fileStrategy: Symlink` on a config that never named a strategy).
/// The null/empty prune above cannot see one, and a per-field
/// `skip_serializing_if` would drop it from every `-o json` payload the same
/// struct serializes into, not just from the user's file. The default is
/// detected without naming any type's fields: the tree is re-parsed as `T`
/// with the key removed, and when it re-serializes to the same tree the key
/// carried nothing but what an absent key reads back as. A key the original
/// file declares is kept whatever its value, so a user who wrote the default
/// out on purpose keeps it. Only scalars are candidates: a mapping or sequence
/// is either data or already pruned as an absent section.
fn prune_undeclared_defaults<T: serde::Serialize + serde::de::DeserializeOwned>(
    tree: &mut serde_yaml::Value,
    declared: &serde_yaml::Value,
) {
    let mut candidates = Vec::new();
    collect_undeclared_scalars(tree, declared, &mut Vec::new(), &mut candidates);
    for path in candidates {
        let mut probe = tree.clone();
        remove_at(&mut probe, &path);
        let Ok(parsed) = serde_yaml::from_value::<T>(probe) else {
            continue;
        };
        let Ok(mut round_trip) = serde_yaml::to_value(&parsed) else {
            continue;
        };
        prune_absent_sections(&mut round_trip, 0);
        if round_trip == *tree {
            remove_at(tree, &path);
        }
    }
}

fn collect_undeclared_scalars(
    tree: &serde_yaml::Value,
    declared: &serde_yaml::Value,
    path: &mut Vec<YamlStep>,
    out: &mut Vec<Vec<YamlStep>>,
) {
    match tree {
        serde_yaml::Value::Mapping(map) => {
            for (key, value) in map {
                path.push(YamlStep::Key(key.clone()));
                // The document's own top-level keys are never candidates.
                let is_scalar = matches!(
                    value,
                    serde_yaml::Value::Bool(_)
                        | serde_yaml::Value::Number(_)
                        | serde_yaml::Value::String(_)
                );
                if is_scalar {
                    if path.len() > 1 && lookup(declared, path).is_none() {
                        out.push(path.clone());
                    }
                } else {
                    collect_undeclared_scalars(value, declared, path, out);
                }
                path.pop();
            }
        }
        serde_yaml::Value::Sequence(seq) => {
            for (index, value) in seq.iter().enumerate() {
                path.push(YamlStep::Index(index));
                collect_undeclared_scalars(value, declared, path, out);
                path.pop();
            }
        }
        _ => {}
    }
}

fn lookup<'a>(tree: &'a serde_yaml::Value, path: &[YamlStep]) -> Option<&'a serde_yaml::Value> {
    path.iter().try_fold(tree, |node, step| match step {
        YamlStep::Key(key) => node.as_mapping()?.get(key),
        YamlStep::Index(index) => node.as_sequence()?.get(*index),
    })
}

fn remove_at(tree: &mut serde_yaml::Value, path: &[YamlStep]) {
    let Some((last, parents)) = path.split_last() else {
        return;
    };
    let parent = parents.iter().try_fold(tree, |node, step| match step {
        YamlStep::Key(key) => node.as_mapping_mut()?.get_mut(key),
        YamlStep::Index(index) => node.as_sequence_mut()?.get_mut(*index),
    });
    if let (Some(map), YamlStep::Key(key)) = (parent.and_then(|p| p.as_mapping_mut()), last) {
        map.remove(key);
    }
}

/// Surface every deprecation message `warn_on_legacy_theme_keys` collected
/// while parsing `cfg` (legacy `theme.overrides.*` keys today; any future
/// `parse_config`-time deprecation lands here too) — this is the one drain
/// point every command boundary that reads the user's real `cfgd.yaml` and
/// owns a `Printer` calls right after a successful load. A thin CLI-scoped
/// alias over `CfgdConfig::drain_deprecations`, kept so every existing
/// command-boundary call site can go on spelling this name; the daemon's own
/// startup / SIGHUP-reload sites call the core method directly, since they
/// parse config in cfgd-core and cannot reach a binary-crate helper.
pub(in crate::cli) fn drain_config_deprecations(printer: &Printer, cfg: &mut CfgdConfig) {
    cfg.drain_deprecations(printer);
}

pub(in crate::cli) fn load_config_and_profile(
    cli: &Cli,
    printer: &Printer,
) -> anyhow::Result<(CfgdConfig, String, ResolvedProfile)> {
    let mut cfg = config::load_config(&cli.config)?;
    drain_config_deprecations(printer, &mut cfg);
    let (profile_name, resolved) = resolve_profile_for(cli, &cfg)?;
    Ok((cfg, profile_name, resolved))
}

/// The profile in force for `cli` against an already-parsed `cfg`: the explicit
/// `--profile`, else the config's active profile, resolved through the profiles
/// directory with the source-delivered-profile decoration on a miss.
///
/// The resolution half of [`load_config_and_profile`], factored out so the
/// run-scoped [`RunContext::config_and_profile`] answers the same question the
/// same way instead of restating the rule beside it.
pub(in crate::cli) fn resolve_profile_for(
    cli: &Cli,
    cfg: &CfgdConfig,
) -> anyhow::Result<(String, ResolvedProfile)> {
    let profile_name = match cli.profile.as_deref() {
        Some(p) => p.to_string(),
        None => cfg.active_profile()?.to_string(),
    };
    match config::resolve_profile(&profile_name, &profiles_dir(cli)) {
        Ok(resolved) => Ok((profile_name, resolved)),
        Err(e) => Err(decorate_profile_not_found(cli, cfg, &profile_name, e)),
    }
}

/// Load config and resolve a profile, with the `--module` isolate mode
/// shared by `cmd_apply` and `cmd_plan`: when `module_filter` names one or
/// more modules and `with_profile` is false, the run is ISOLATED from the
/// active profile unconditionally — a profile is never even resolved, so a
/// profile that DOES resolve can no longer leak its packages/files/env/
/// aliases/system/scripts into a run the caller asked to isolate. `--module
/// --with-profile` (or no `--module` at all) behaves exactly like a normal
/// run: the active profile must resolve, same as `cfgd apply` with no flags.
///
/// Loads (and drains) `cli.config` EXACTLY ONCE regardless of which branch
/// is taken. The two call sites this replaces each called
/// `load_config_and_profile` (load + drain #1), and on its `Err` re-parsed
/// the same file a second time to build the module-only fallback (load +
/// drain #2) — the same legacy-key deprecation notice landing on the
/// user's terminal twice for one `apply --module x` / `plan --module x`
/// invocation.
pub(in crate::cli) fn load_config_and_profile_module_scoped(
    cli: &Cli,
    printer: &Printer,
    module_filter: &[String],
    with_profile: bool,
) -> anyhow::Result<(CfgdConfig, ResolvedProfile, Option<String>, bool)> {
    if module_filter.is_empty() || with_profile {
        let (cfg, profile_name, resolved) = load_config_and_profile(cli, printer)?;
        return Ok((cfg, resolved, Some(profile_name), true));
    }

    // `minimal_config()` subscribes to nothing, and that fabricated empty
    // list must never reach the decision sweep: it would read as "no
    // source is subscribed any more" and delete every decision row on the
    // machine, turning "awaiting your answer" into "applies silently" with
    // nothing to recover from.
    let (cfg, config_parsed) = match config::load_config(&cli.config) {
        Ok(mut cfg) => {
            drain_config_deprecations(printer, &mut cfg);
            (cfg, true)
        }
        Err(_) => (config::minimal_config(), false),
    };

    // Isolation skips composing the profile's CONTENT, but an explicit
    // `--profile` still names a real thing every module script reads back as
    // `CFGD_PROFILE` — a typo here must not silently become the literal
    // string a script trusts. `active_profile_name`'s own fallback path (no
    // `--profile`, config's own `active_profile()`) stays best-effort: that
    // name was never user-typed on THIS invocation, so there is no operator
    // input to validate.
    if let Some(name) = cli.profile.as_deref()
        && let Err(e) = config::resolve_profile(name, &profiles_dir(cli))
    {
        return Err(decorate_profile_not_found(cli, &cfg, name, e));
    }

    let resolved = empty_resolved_profile(module_filter, &active_profile_name(cli, Some(&cfg)));
    Ok((cfg, resolved, None, config_parsed))
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
            "Profile '{profile_name}' is delivered by {}: {providers_list}. The active/selected profile must be a LOCAL profile; wrap the source profile in one.",
            cfgd_core::plural_noun(providers.len(), "source")
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

/// One resolved `--package` token.
///
/// Three names, because three different surfaces need three different ones and
/// collapsing any two of them is how a token lands somewhere the user cannot
/// find it: `schema_path` is what the user typed and what a confirmation line
/// echoes back (`brew.taps`), `slot` is the key the package spec is written
/// through, and `manager` is the REGISTERED manager name — the only one of the
/// three that is ever persisted as a manager (a module entry's `prefer`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::cli) struct PackageRef {
    /// `None` for a bare name, which takes the platform's native manager.
    pub schema_path: Option<String>,
    pub slot: Option<String>,
    pub manager: Option<String>,
    pub name: String,
}

impl PackageRef {
    /// The key [`crate::packages::add_package`] / `remove_package` write through.
    pub(in crate::cli) fn slot_or<'a>(&'a self, native: &'a str) -> &'a str {
        self.slot.as_deref().unwrap_or(native)
    }

    /// The word a confirmation line calls this entry — `Added tap:` for
    /// `brew.taps`, `Added cask:` for `brew.casks`, `Added package:` for
    /// everything else including a bare name and a custom manager.
    ///
    /// Read off the schema table rather than decided here, so the add verb and
    /// the remove verb cannot disagree and a new sub-list gets its noun from
    /// one place.
    pub(in crate::cli) fn noun(&self) -> &'static str {
        self.schema_path
            .as_deref()
            .and_then(cfgd_core::config::package_schema_path)
            .map_or(cfgd_core::config::DEFAULT_PACKAGE_NOUN, |p| p.noun())
    }

    /// [`Self::noun`] opening a sentence (`Tap 'x' not found in brew.taps`).
    /// Every noun in the table is one plain ASCII word.
    pub(in crate::cli) fn noun_capitalized(&self) -> String {
        let noun = self.noun();
        let mut chars = noun.chars();
        match chars.next() {
            Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
            None => noun.to_string(),
        }
    }

    /// `charmbracelet/tap (brew.taps)` — the schema path the value was written
    /// to, so the confirmation names a path the user can go and look at.
    pub(in crate::cli) fn display(&self, native: &str) -> String {
        format!(
            "{} ({})",
            self.name,
            self.schema_path.as_deref().unwrap_or(native)
        )
    }
}

/// Parse a `--package` flag value into the schema path it names.
///
/// ```text
/// --package <manager>[.<list>]:<name>   brew:ripgrep  brew.taps:charmbracelet/tap  apt:libc6:amd64
/// --package <name>                      the platform's native manager
/// ```
///
/// A colon-carrying token whose prefix names nothing is an ERROR, never a bare
/// name: `--package brew.tap:charmbracelet/tap` used to be accepted as an apt
/// package literally called `brew.tap:charmbracelet/tap`, which installs
/// nothing and is discovered only when the apply fails. The name keeps every
/// colon after the first, so `apt:libc6:amd64` is the apt package `libc6:amd64`.
pub(in crate::cli) fn parse_package_flag(
    s: &str,
    custom_managers: &[String],
    native: &str,
) -> anyhow::Result<PackageRef> {
    let Some((prefix, name)) = s.split_once(':') else {
        return Ok(PackageRef {
            schema_path: None,
            slot: None,
            manager: None,
            name: s.to_string(),
        });
    };
    if prefix.is_empty() || name.is_empty() {
        anyhow::bail!(
            "invalid package '--package {s}' — expected <manager>[.<list>]:<name> or a bare name"
        );
    }
    if let Some(path) = cfgd_core::config::package_schema_path(prefix) {
        return Ok(PackageRef {
            schema_path: Some(path.path.to_string()),
            slot: Some(path.slot.to_string()),
            manager: Some(path.manager.to_string()),
            name: name.to_string(),
        });
    }
    if custom_managers.iter().any(|c| c == prefix) {
        return Ok(PackageRef {
            schema_path: Some(prefix.to_string()),
            slot: Some(prefix.to_string()),
            manager: Some(prefix.to_string()),
            name: name.to_string(),
        });
    }
    Err(unknown_package_prefix(
        s,
        prefix,
        name,
        custom_managers,
        native,
    ))
}

/// The `--package` tokens that WOULD remove `name` from `packages`, for a bare
/// removal that resolved to the native manager and found nothing there.
///
/// A bare token means "the platform's native manager", so `--package -ripgrep`
/// on a Debian host looks in `apt` alone and reports a miss for a package
/// sitting in `brew.formulae`. Naming the token that works is the only useful
/// answer; one path per write SLOT, since two paths reaching the same list
/// (`apt` and `apt.packages`) would offer the same removal twice.
pub(in crate::cli) fn removal_tokens_for(
    name: &str,
    packages: &cfgd_core::config::PackagesSpec,
) -> Vec<String> {
    let mut seen: Vec<&str> = Vec::new();
    let mut tokens = Vec::new();
    for entry in cfgd_core::config::PACKAGE_SCHEMA_PATHS {
        if seen.contains(&entry.slot) {
            continue;
        }
        seen.push(entry.slot);
        if cfgd_core::config::desired_packages_for_spec(entry.slot, packages)
            .iter()
            .any(|p| p == name)
        {
            tokens.push(format!("--package -{}:{name}", entry.path));
        }
    }
    for custom in &packages.custom {
        if custom.packages.iter().any(|p| p == name) {
            tokens.push(format!("--package -{}:{name}", custom.name));
        }
    }
    tokens
}

/// The error a `--package` prefix that names nothing gets, with whichever hint
/// its shape earns.
fn unknown_package_prefix(
    token: &str,
    prefix: &str,
    name: &str,
    custom_managers: &[String],
    native: &str,
) -> anyhow::Error {
    // A REGISTERED manager name that is not a schema path is one of the two
    // virtual brew managers: the user reached for the wire spelling, and the
    // schema spells the same thing differently.
    if let Some(path) = cfgd_core::config::PACKAGE_SCHEMA_PATHS
        .iter()
        .find(|p| p.slot == prefix && p.path != prefix)
    {
        return anyhow::anyhow!(
            "unknown package manager '{prefix}' in '--package {token}'; \
             use {}:{name}",
            path.path
        );
    }
    let mut known: Vec<String> = cfgd_core::config::PACKAGE_SCHEMA_PATHS
        .iter()
        .map(|p| p.path.to_string())
        .collect();
    known.extend(custom_managers.iter().cloned());
    known.sort();
    let known = known.join(", ");
    // A prefix whose first dot-segment names no manager either is not a
    // mis-spelled schema path at all — it is part of the package's own name
    // (`libc6:amd64`), and what the user wants is the native manager in front.
    let manager_shaped = cfgd_core::config::package_schema_path(
        prefix
            .split_once('.')
            .map(|(head, _)| head)
            .unwrap_or(prefix),
    )
    .is_some();
    if manager_shaped {
        anyhow::anyhow!("unknown package manager '{prefix}' in '--package {token}'; known: {known}")
    } else {
        anyhow::anyhow!(
            "unknown package manager '{prefix}' in '--package {token}'; \
             did you mean {native}:{token}? (known: {known})"
        )
    }
}

/// Best-effort name of the profile a module-only command runs under: the
/// explicit `--profile`, else the config's active profile, else `"unknown"`.
///
/// Module-only commands never resolve a profile, but the scripts they run
/// (a `patch.script` filter, a lifecycle hook) still receive `CFGD_PROFILE`,
/// so the name must be the real one wherever the config knows it. Pass `cfg`
/// when it is already loaded to avoid a second read.
/// The `spec.backups[]` units an apply runs unconditionally.
///
/// A schedule-less unit has no timer to fire it, so every non-dry-run apply is
/// its trigger. `plan` and `apply` share this so the preview can never list work
/// the run then skips, or omit work the run then does.
pub(in crate::cli) fn pending_backups(
    merged: &cfgd_core::config::MergedProfile,
) -> Vec<&cfgd_core::config::BackupSpec> {
    merged
        .backups
        .iter()
        .filter(|b| b.schedule.is_none())
        .collect()
}

pub(in crate::cli) fn active_profile_name(cli: &Cli, cfg: Option<&CfgdConfig>) -> String {
    if let Some(p) = cli.profile.as_deref() {
        return p.to_string();
    }
    let from_loaded = cfg.and_then(|c| c.active_profile().ok().map(str::to_string));
    from_loaded
        .or_else(|| {
            config::load_config(&cli.config)
                .ok()
                .and_then(|c| c.active_profile().ok().map(str::to_string))
        })
        .unwrap_or_else(|| "unknown".to_string())
}

/// Build an empty ResolvedProfile for module-only operations that don't need
/// a real profile (status --module, verify --module, apply --module without profile).
///
/// The single synthesized layer exists to carry `profile_name`: it is what
/// [`ResolvedProfile::profile_name`] reports, and therefore what a module's
/// scripts see as `CFGD_PROFILE`. Its spec is empty, so the layer contributes
/// nothing to the merged profile.
pub(in crate::cli) fn empty_resolved_profile(
    module_names: &[String],
    profile_name: &str,
) -> ResolvedProfile {
    ResolvedProfile {
        layers: vec![cfgd_core::config::ProfileLayer {
            source: cfgd_core::config::LOCAL_LAYER.to_string(),
            profile_name: profile_name.to_string(),
            priority: 0,
            policy: cfgd_core::config::LayerPolicy::Local,
            spec: cfgd_core::config::ProfileSpec::default(),
        }],
        merged: MergedProfile {
            modules: module_names.to_vec(),
            ..Default::default()
        },
    }
}

pub(in crate::cli) use cfgd_core::reconciler::{CFGD_BACKUP_SUFFIX, cfgd_backup_path};

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
                "Workflow regeneration failed ({}); the on-disk workflow is stale until this is resolved and the workflow is regenerated",
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

/// The ONE state directory a run uses — `state.db`, the apply mutex
/// (`apply.lock`), and everything else state-rooted resolve HERE.
///
/// The apply mutex serializes the only operation that mutates live system
/// state, so it co-locates with the `state.db` it guards — the same dir the
/// daemon reconcile loop locks — and every acquirer must resolve it identically
/// (`--state-dir` flag > `CFGD_STATE_DIR` env > `XDG_STATE_HOME` > platform
/// default, per the run's `--scope`) regardless of how the process was
/// launched, or the lock fails to mutually-exclude and concurrent applies
/// corrupt state. `open_state_store` resolves through the same call for the
/// same reason: a `--scope system` run that locked the system dir while
/// opening the user store would judge ownership against one store and sweep
/// another.
pub(crate) fn run_state_dir(
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
    let mut cfg = config::load_config(config_path)?;
    drain_config_deprecations(printer, &mut cfg);
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
    /// The config-aware provider registry that maps module packages onto
    /// managers — the ONE registry a composing run needs, handed back rather
    /// than rebuilt by the caller. Reached through [`Self::take_registry`],
    /// which builds it if the resolution did not already need it, so a command
    /// that never asks (`decide` classifies sources and reads no manager) pays
    /// nothing. `set_system_config_dir` is the caller's to apply: the read paths
    /// that never set it must keep not setting it.
    registry: std::cell::OnceCell<ProviderRegistry>,
    /// The sources this composition actually drew a layer from, in layering
    /// order. Read off the composed layers rather than off `cfg.spec.sources`,
    /// so a subscription that contributed nothing is not announced as though it
    /// had, and the profile named is the one that really merged.
    pub sources: Vec<cfgd_core::reconciler::ComposedSource>,
    pub source_env: std::collections::HashMap<String, Vec<cfgd_core::config::EnvVar>>,
    pub source_commits: std::collections::HashMap<String, String>,
    /// Source security-constraint violations surfaced when the caller composed in
    /// [`cfgd_core::composition::ConstraintMode::Report`] (read paths). Empty for `Enforce` callers
    /// (apply/plan), which abort on the first violation instead.
    pub constraint_violations: Vec<cfgd_core::composition::ConstraintViolation>,
}

impl DesiredState {
    /// Whether the resolution itself already built the registry.
    #[cfg(test)]
    pub(in crate::cli) fn registry_built(&self) -> bool {
        self.registry.get().is_some()
    }

    /// Take the run's config-aware registry, building it on first ask.
    ///
    /// `cfg` is a parameter rather than a field because the registry's other
    /// input is `self.resolved.merged.packages` — a sibling field, which no
    /// `OnceCell` initializer stored beside it could borrow. Owned, because
    /// every caller mutates what it gets (`set_system_config_dir`) or hands it
    /// to a `Reconciler` that wants it by value.
    pub(in crate::cli) fn take_registry(&mut self, cfg: &config::CfgdConfig) -> ProviderRegistry {
        self.registry.take().unwrap_or_else(|| {
            build_registry_with_config_and_packages(Some(cfg), Some(&self.resolved.merged.packages))
        })
    }
}

/// Compose the local profile with configured sources into an effective profile.
///
/// `refresh = true` fetches each source over the network (write paths:
/// `apply`/`plan`); `refresh = false` loads sources from their on-disk cache and
/// never touches the network (read paths). Delegates the actual merge to the
/// single composition code path in [`SourceManager::compose`], then displays and
/// persists any conflicts.
pub(in crate::cli) fn compose_with_sources(
    ctx: &RunContext<'_>,
    cfg: &config::CfgdConfig,
    local_resolved: &ResolvedProfile,
    printer: &Printer,
    refresh: bool,
    mode: composition::ConstraintMode,
) -> anyhow::Result<composition::CompositionResult> {
    let cli = ctx.cli();
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
    mgr.set_announce_cache_skips(ctx.announce_cache_skips());
    if refresh {
        mgr.load_sources(&cfg.spec.sources, printer)?;
    } else {
        // Read paths stay offline: load from cache, warn+skip never-synced sources.
        mgr.load_sources_cached(&cfg.spec.sources, printer)?;
    }

    let result = mgr.compose(&cfg.spec.sources, local_resolved, mode)?;
    display_and_persist_conflicts(ctx, &result, printer);

    // Report mode accumulates violations instead of aborting; without this the
    // only surface that ever showed them was a compliance snapshot, so an
    // operator running `diff` saw a source's contribution rendered with no sign
    // that it breaks the source's own constraints.
    for violation in &result.constraint_violations {
        printer
            .status(
                Role::Warn,
                format!(
                    "Source '{}' violates its constraints",
                    violation.source_name
                ),
            )
            .detail(&violation.detail);
    }

    // Surface the documented "scripts are shown in cfgd plan" promise: when a
    // subscriber opted in (`allowScripts: true`) to a source whose
    // `constraints.no_scripts` would otherwise block scripts, the script
    // execution must be visible. Naming the concrete surfaces matters because
    // two of the three do not look like lifecycle scripts from the outside: a
    // backup hook runs on the daemon's timer, and a patch filter runs on
    // read-only commands too.
    //
    // A `Warn` status, not `note`: a note renders only at `-v`, which is not a
    // place to put the one line telling an operator that third-party code is
    // about to run on their machine. Non-fatal — the opt-in already permitted it.
    for spec in &cfg.spec.sources {
        if spec.subscription.allow_scripts
            && let Some(cached) = mgr.get(&spec.name)
            && cached.manifest.spec.policy.constraints.no_scripts
        {
            let surfaces: Vec<String> = result
                .resolved
                .layers
                .iter()
                .filter(|layer| layer.source == spec.name)
                .flat_map(|layer| composition::script_surfaces(&layer.spec))
                .collect();
            // A source that ships no script surface has nothing to disclose;
            // announcing that its scripts "will run" would name a risk the
            // subscriber does not actually carry.
            if surfaces.is_empty() {
                continue;
            }
            printer
                .status(
                    Role::Warn,
                    format!(
                        "Source '{}' scripts will run because `allowScripts` is set",
                        spec.name
                    ),
                )
                .detail(format!(
                    "constraints.noScripts is overridden by your subscription; it carries {}",
                    surfaces.join(", ")
                ));
        }
    }

    Ok(result)
}

/// Reword `conflict.details`'s persisted `" <- "` arrow to `" from "` for
/// terminal display. `conflict.details` is `composition::record`'s
/// persisted string and keeps its own `<-` shape in storage (see that
/// module's doc comment); this is the ONE display-side reword, shared by
/// `display_and_persist_conflicts` below and `source::helpers::format_conflict_preview_lines`,
/// so a raw ASCII arrow reaches the terminal from neither surface.
pub(in crate::cli) fn reword_conflict_arrow_for_display(details: &str) -> String {
    details.replace(" <- ", " from ")
}

/// Render composition conflicts under a section and persist them to the state
/// store for `status`/history. Best-effort persistence: a state error is logged,
/// not fatal, so a read-only filesystem never blocks a compose.
fn display_and_persist_conflicts(
    ctx: &RunContext<'_>,
    result: &composition::CompositionResult,
    printer: &Printer,
) {
    if result.conflicts.is_empty() {
        return;
    }
    let guard = printer.section("Source Conflicts");
    for conflict in &result.conflicts {
        let role = match conflict.resolution_type {
            composition::ResolutionType::Locked => Role::Warn,
            composition::ResolutionType::Required
            | composition::ResolutionType::Rejected
            | composition::ResolutionType::Override => Role::Info,
            // A default resolution settled itself; nothing to tell the operator.
            composition::ResolutionType::Default => continue,
        };
        guard.status_simple(role, reword_conflict_arrow_for_display(&conflict.details));
    }
    drop(guard);

    if let Some(state) = ctx.state_opt() {
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
/// `module_filter` scopes module resolution to the named modules
/// (apply/plan `--module`, repeatable); empty resolves the whole effective
/// profile. A non-empty `module_filter` with `with_profile = false`
/// ISOLATES: the returned `resolved` is replaced outright by a zeroed
/// profile carrying only those module names, so every profile-owned
/// contribution is zeroed, not just packages/files. `with_profile = true`
/// instead UNIONS the named modules into the full composed profile's own
/// module list, so an out-of-profile module can be added without dropping
/// anything the profile already declares.
///
/// Errors from `compose` (constraint violations, malformed cached manifest,
/// failed signature) propagate so an invalid source config fails every command
/// consistently — a command that reports state must not silently report empty
/// when the desired state is broken. Module-resolution errors (including a
/// genuinely unknown `--module` name, or a source's `ScriptsNotAllowed`
/// constraint) propagate the same way — atomically over the whole requested
/// list, never swallowed to an empty result.
#[allow(clippy::too_many_arguments)]
pub(in crate::cli) fn resolve_desired_state(
    ctx: &RunContext<'_>,
    cfg: &config::CfgdConfig,
    local_resolved: &ResolvedProfile,
    module_filter: &[String],
    with_profile: bool,
    printer: &Printer,
    refresh: bool,
    mode: composition::ConstraintMode,
) -> anyhow::Result<DesiredState> {
    let cli = ctx.cli();
    let composition = compose_with_sources(ctx, cfg, local_resolved, printer, refresh, mode)?;
    let composition::CompositionResult {
        resolved,
        source_env,
        source_commits,
        source_module_roots,
        constraint_violations,
        ..
    } = composition;

    // Taken BEFORE the module isolation below replaces `resolved` with a zeroed
    // profile: that replacement drops every layer, and the header would then
    // stop naming the sources a `--module` run still composed its module roots
    // from.
    let sources = cfgd_core::reconciler::ComposedSource::from_profile_layers(&resolved.layers);

    let config_dir = ctx.config_dir();

    // `--module` without `--with-profile` isolates: only the named modules
    // (plus their dependencies) plan. `--with-profile` keeps the fully
    // composed profile and UNIONS the named modules into its own module
    // list instead of replacing it, so an out-of-profile module can be
    // added without dropping any module the profile already declares.
    let module_names: Vec<String> = if module_filter.is_empty() {
        resolved.merged.modules.clone()
    } else if with_profile {
        let mut names = resolved.merged.modules.clone();
        for name in module_filter {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
        names
    } else {
        module_filter.to_vec()
    };

    // The isolation itself: replace the whole composed profile with a zeroed
    // one carrying only the requested module names, so every profile-owned
    // contribution — packages, files, env, aliases, system, scripts,
    // secrets, backups — is zeroed regardless of whether a profile resolved
    // or a source layered its own content into the composition above. A
    // half-isolation that zeroed only packages/files (leaving env/aliases/
    // system/scripts to flow through from `resolved.merged`) is exactly the
    // bug this replaces: `Reconciler::plan` derives ALL of those from
    // `resolved.merged`, not just packages/files.
    // `desired.modules` (built from `module_names` below) is what
    // `Reconciler::plan` actually resolves against — `resolved.merged.modules`
    // itself is never read there. It IS read by callers outside the plan/apply
    // path that take a `ResolvedProfile` at face value (the daemon's own
    // resolution, `cfgd doctor`, `cfgd module list/show`, `cfgd init`), so it
    // must still agree with what this run resolved: a `--with-profile` caller
    // reading it back would otherwise see the profile's ORIGINAL module list
    // with the named module silently missing from it, even though that module
    // is what `desired.modules` — and therefore the plan — actually carries.
    let mut resolved = if !module_filter.is_empty() && !with_profile {
        empty_resolved_profile(module_filter, &active_profile_name(cli, Some(cfg)))
    } else {
        resolved
    };
    if !module_filter.is_empty() && with_profile {
        resolved.merged.modules = module_names.clone();
    }

    // Config-aware registry so a module that references a custom package manager
    // (declared in cfg / composed packages) resolves identically on every
    // command — matching the apply path's registry. Filled HERE when the module
    // walk needs it and handed back on `DesiredState` either way, so the caller
    // reuses it: every command that composes a desired state then built a
    // second, identical registry of its own, and a registry build constructs
    // every package manager and every configurator this host supports.
    //
    // `build_registry_with_config_and_packages` already registers the spec's
    // custom managers; the second `extend_package_managers` this replaced added
    // each of them a SECOND time, so `package_managers()` answered with two
    // entries per custom manager.
    let registry: std::cell::OnceCell<ProviderRegistry> = std::cell::OnceCell::new();

    let modules = if module_names.is_empty() {
        Vec::new()
    } else {
        let platform = Platform::current();
        let mgr_map = registry
            .get_or_init(|| {
                build_registry_with_config_and_packages(Some(cfg), Some(&resolved.merged.packages))
            })
            .manager_map();
        let cache_base = module_cache_dir(cli)?;
        // The run's own installed-state memo: a bare entry resolves to the
        // manager that already holds it, and the plan's elision then reads
        // the same enumeration. A run with no state to open keeps the
        // platform default.
        let pkg_cx = ctx.package_context().ok();
        // Resolution is atomic over the whole requested-name list: any
        // failure — including a genuinely unknown module name, or a source
        // constraint (`ScriptsNotAllowed`) — propagates as the error it is.
        // "Not found" is reserved for a name that truly does not resolve;
        // swallowing every error here as a silent empty Vec previously made
        // a source-constraint violation on `--module x` report as "module
        // not found" instead of the constraint failure it actually was.
        modules::resolve_modules(
            &module_names,
            config_dir,
            &cache_base,
            &source_module_roots,
            platform,
            &mgr_map,
            pkg_cx.as_ref(),
            printer,
        )?
    };

    Ok(DesiredState {
        resolved,
        modules,
        registry,
        sources,
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
            printer
                .status(Role::Warn, "No git remote/HEAD detected")
                .detail("SLSA provenance will record source as \"unknown\"");
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

pub(in crate::cli) use cfgd_core::short_commit;

#[cfg(test)]
pub(crate) mod tests;
