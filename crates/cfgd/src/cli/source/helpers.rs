use super::*;

// --- Source cache layout ---

pub(crate) fn source_cache_dir(cli: &Cli) -> anyhow::Result<std::path::PathBuf> {
    if let Some(ref state_dir) = cli.state_dir {
        Ok(state_dir.join("sources"))
    } else {
        SourceManager::default_cache_dir().map_err(|e| anyhow::anyhow!(e))
    }
}

// --- Composition input builder ---

/// Build a minimal [`CompositionInput`] from a source policy for permission change detection.
/// Only the `source_name`, `policy`, and `constraints` fields are used by
/// [`composition::detect_permission_changes`]; the rest are defaulted.
pub(crate) fn build_permission_input(
    name: &str,
    policy: &config::ConfigSourcePolicy,
) -> CompositionInput {
    CompositionInput {
        source_name: name.to_string(),
        priority: 0,
        policy: policy.clone(),
        constraints: policy.constraints.clone(),
        layers: Vec::new(),
        subscription: SubscriptionConfig::default(),
    }
}

// --- Source config helpers ---

pub(crate) fn infer_source_name(url: &str) -> String {
    // Extract name from URL: git@github.com:acme/dev-config.git -> acme-dev-config
    let cleaned = url
        .trim_end_matches(".git")
        .rsplit('/')
        .next()
        .or_else(|| url.rsplit(':').next())
        .unwrap_or(url);

    // If the path component includes org/repo, use org-repo
    if let Some(rest) = url.strip_prefix("git@")
        && let Some(path) = rest.split(':').nth(1)
    {
        return path.trim_end_matches(".git").replace('/', "-");
    }

    cleaned.to_string()
}

/// Default priority assigned to a non-interactive `cfgd source add --yes` run
/// when neither `--priority` nor an interactive prompt picks one. Pinned at
/// the midpoint of the 1–1000 priority space so non-interactive subscriptions
/// don't implicitly outrank or sit beneath user-curated sources.
pub(crate) const DEFAULT_NONINTERACTIVE_PRIORITY: u32 = 500;

/// Pick the source-add profile without consulting the user. Returns:
/// * `Some(name)` when an explicit `--profile`, an auto-detected platform
///   profile, or a sole-option profile decides the choice.
/// * `None` when the caller must either prompt (multiple options) or accept
///   "no profile" — the caller distinguishes the two by checking
///   `provided_profiles.is_empty()`.
pub(crate) fn resolve_non_interactive_profile(
    explicit: Option<&str>,
    auto_detected: Option<&str>,
    provided_profiles: &[String],
) -> Option<String> {
    if let Some(p) = explicit {
        return Some(p.to_string());
    }
    if let Some(p) = auto_detected {
        return Some(p.to_string());
    }
    if provided_profiles.len() == 1 {
        return Some(provided_profiles[0].clone());
    }
    None
}

/// Parse the priority text typed at the interactive `cfgd source add` prompt.
/// Surfaces the canonical `invalid priority: '<input>' (must be a number)`
/// error so the wording stays in lockstep with the user-facing CLI.
pub(crate) fn parse_priority_input(input: &str) -> anyhow::Result<u32> {
    input
        .parse::<u32>()
        .map_err(|_| anyhow::anyhow!("invalid priority: '{}' (must be a number)", input))
}

/// Print the "Source Manifest" + "Policy" sections of `cfgd source add` and
/// return the list of profile names the manifest provides. Caller uses the
/// returned list both for downstream profile-selection and to skip the
/// interactive prompt when the source provides exactly one profile.
///
/// Side effects only on `printer` — split out so the output contract is
/// testable via `Printer::for_test()` without needing a real source clone.
pub(crate) fn display_source_manifest(
    printer: &Printer,
    manifest: &config::ConfigSourceDocument,
) -> Vec<String> {
    printer.subheader("Source Manifest");
    printer.key_value("Name", &manifest.metadata.name);
    if let Some(ref version) = manifest.metadata.version {
        printer.key_value("Version", version);
    }
    if let Some(ref desc) = manifest.metadata.description {
        printer.key_value("Description", desc);
    }

    let provided_profiles = cfgd_core::config::source_profile_names(&manifest.spec.provides);
    if !provided_profiles.is_empty() {
        printer.key_value("Profiles", &provided_profiles.join(", "));
    }

    // Policy summary
    let policy = &manifest.spec.policy;
    let required_count = count_policy_items(&policy.required);
    let recommended_count = count_policy_items(&policy.recommended);
    let locked_count = count_policy_items(&policy.locked);

    printer.newline();
    printer.subheader("Policy");
    if locked_count > 0 {
        printer.warning(&format!(
            "{} locked item(s) (cannot override)",
            locked_count
        ));
    }
    if required_count > 0 {
        printer.info(&format!(
            "{} required item(s) (team requirement)",
            required_count
        ));
    }
    if recommended_count > 0 {
        printer.info(&format!("{} recommended item(s)", recommended_count));
    }

    // Constraints
    let constraints = &manifest.spec.policy.constraints;
    if constraints.no_scripts {
        printer.info("Scripts: blocked");
    }
    if constraints.no_secrets_read {
        printer.info("Secret access: blocked");
    }
    if !constraints.allowed_target_paths.is_empty() {
        printer.info(&format!(
            "Allowed paths: {}",
            constraints.allowed_target_paths.join(", ")
        ));
    }

    provided_profiles
}

pub(crate) fn count_policy_items(items: &config::PolicyItems) -> usize {
    let mut count = 0;
    if let Some(ref pkgs) = items.packages {
        if let Some(ref brew) = pkgs.brew {
            count += brew.formulae.len() + brew.casks.len() + brew.taps.len();
        }
        if let Some(ref apt) = pkgs.apt {
            count += apt.packages.len();
        }
        if let Some(ref cargo) = pkgs.cargo {
            count += cargo.packages.len();
        }
        count += pkgs.pipx.len() + pkgs.dnf.len();
        if let Some(ref npm) = pkgs.npm {
            count += npm.global.len();
        }
    }
    count += items.files.len();
    count += items.env.len();
    count += items.system.len();
    count
}

pub(crate) fn display_policy_items(printer: &Printer, items: &config::PolicyItems, indent: &str) {
    if let Some(ref pkgs) = items.packages {
        if let Some(ref brew) = pkgs.brew {
            for f in &brew.formulae {
                printer.info(&format!("{indent}brew formula: {f}"));
            }
            for c in &brew.casks {
                printer.info(&format!("{indent}brew cask: {c}"));
            }
        }
        if let Some(ref apt) = pkgs.apt {
            for p in &apt.packages {
                printer.info(&format!("{indent}apt: {p}"));
            }
        }
        if let Some(ref cargo) = pkgs.cargo {
            for p in &cargo.packages {
                printer.info(&format!("{indent}cargo: {p}"));
            }
        }
        for p in &pkgs.pipx {
            printer.info(&format!("{indent}pipx: {p}"));
        }
        for p in &pkgs.dnf {
            printer.info(&format!("{indent}dnf: {p}"));
        }
        if let Some(ref npm) = pkgs.npm {
            for p in &npm.global {
                printer.info(&format!("{indent}npm: {p}"));
            }
        }
    }
    for f in &items.files {
        printer.info(&format!("{indent}file: {}", f.target.display()));
    }
    for ev in &items.env {
        printer.info(&format!("{indent}env: {}", ev.name));
    }
    for k in items.system.keys() {
        printer.info(&format!("{indent}system: {k}"));
    }
}

pub(crate) fn display_pending_decisions(
    printer: &Printer,
    decisions: &[cfgd_core::state::PendingDecision],
) {
    let mut by_source: std::collections::BTreeMap<&str, Vec<&cfgd_core::state::PendingDecision>> =
        std::collections::BTreeMap::new();
    for d in decisions {
        by_source.entry(&d.source).or_default().push(d);
    }
    for (source_name, items) in &by_source {
        printer.info(&format!(
            "{}: {} pending item{}",
            source_name,
            items.len(),
            if items.len() == 1 { "" } else { "s" }
        ));
        for item in items {
            printer.info(&format!(
                "  {} {} — {} ({})",
                item.tier, item.resource, item.summary, item.action
            ));
        }
    }
}

pub(crate) fn add_source_to_config(
    config_path: &Path,
    source: &config::SourceSpec,
) -> anyhow::Result<()> {
    if !config_path.exists() {
        anyhow::bail!("Config file not found: {}", config_path.display());
    }

    mutate_config_yaml(config_path, true, |raw| {
        let spec = raw
            .get_mut("spec")
            .ok_or_else(|| anyhow::anyhow!("config missing 'spec'"))?;
        let sources = spec
            .as_mapping_mut()
            .ok_or_else(|| anyhow::anyhow!("spec is not a mapping"))?
            .entry(serde_yaml::Value::String("sources".into()))
            .or_insert(serde_yaml::Value::Sequence(vec![]));
        let seq = sources
            .as_sequence_mut()
            .ok_or_else(|| anyhow::anyhow!("sources is not a sequence"))?;
        let source_value = serde_yaml::to_value(source)?;
        seq.push(source_value);
        Ok(())
    })
}

pub(crate) fn remove_source_from_config(config_path: &Path, name: &str) -> anyhow::Result<()> {
    if !config_path.exists() {
        return Ok(());
    }
    mutate_config_yaml(config_path, true, |raw| {
        if let Some(spec) = raw.get_mut("spec")
            && let Some(sources) = spec.get_mut("sources")
            && let Some(seq) = sources.as_sequence_mut()
        {
            seq.retain(|item| {
                item.get("name")
                    .and_then(|n| n.as_str())
                    .map(|n| n != name)
                    .unwrap_or(true)
            });
        }
        Ok(())
    })
}

fn find_source_in_config<'a>(
    raw: &'a mut serde_yaml::Value,
    source_name: &str,
) -> Option<&'a mut serde_yaml::Value> {
    raw.get_mut("spec")?
        .get_mut("sources")?
        .as_sequence_mut()?
        .iter_mut()
        .find(|item| {
            item.get("name")
                .and_then(|n| n.as_str())
                .map(|n| n == source_name)
                .unwrap_or(false)
        })
}

/// Generalized read-parse-mutate-write loop for `cfgd.yaml`.
///
/// Loads the YAML at `config_path`, hands the mutable root `serde_yaml::Value`
/// to `f`, then serializes and atomically writes the result. When `validate`
/// is `true`, the serialized output is round-tripped through
/// `config::parse_config` before write — callers that could produce schema-invalid
/// documents (`set`, `unset`) pass `true`; mechanical add/remove-by-key
/// operations pass `false` so the write path is free of the typed-parse cost.
///
/// Use this instead of open-coding the `read_to_string → from_str → mutate →
/// to_string → atomic_write_str` pattern, which diverged in validation
/// behavior (set/unset validated; add/remove did not) before this helper.
pub(crate) fn mutate_config_yaml<F>(config_path: &Path, validate: bool, f: F) -> anyhow::Result<()>
where
    F: FnOnce(&mut serde_yaml::Value) -> anyhow::Result<()>,
{
    let contents = std::fs::read_to_string(config_path)?;
    let mut raw: serde_yaml::Value = serde_yaml::from_str(&contents)?;
    f(&mut raw)?;
    let output = serde_yaml::to_string(&raw)?;
    if validate {
        config::parse_config(&output, config_path)
            .map_err(|e| anyhow::anyhow!("config would become invalid: {}", e))?;
    }
    cfgd_core::atomic_write_str(config_path, &output)?;
    Ok(())
}

/// Load config YAML, find a named source, apply a mutation, and write back.
/// The closure receives the mutable source entry; the helper handles I/O.
pub(super) fn with_source_config<F>(
    config_path: &Path,
    source_name: &str,
    f: F,
) -> anyhow::Result<()>
where
    F: FnOnce(&mut serde_yaml::Value) -> anyhow::Result<()>,
{
    mutate_config_yaml(config_path, false, |raw| {
        let source = find_source_in_config(raw, source_name)
            .ok_or_else(|| anyhow::anyhow!("source '{}' not found in config file", source_name))?;
        f(source)
    })
}

// --- Conflict-preview helpers (cmd_source_add) ---

/// Build the [`CompositionInput`] used by `cfgd source add`'s conflict-preview
/// step. The prospective subscription is modeled as a single composition input
/// against the user's current resolved profile — the engine then surfaces the
/// resource-level conflicts that would arise if the subscription went live.
///
/// Pure constructor — split out so the input shape (which fields flow through,
/// which default) is testable without a live SourceManager.
pub(crate) fn build_subscription_preview_input(
    source_name: &str,
    priority: u32,
    manifest_policy: &config::ConfigSourcePolicy,
    accept_recommended: bool,
    opt_in: &[String],
    layers: Vec<config::ProfileLayer>,
) -> CompositionInput {
    CompositionInput {
        source_name: source_name.to_string(),
        priority,
        policy: manifest_policy.clone(),
        constraints: manifest_policy.constraints.clone(),
        layers,
        subscription: SubscriptionConfig {
            accept_recommended,
            opt_in: opt_in.to_vec(),
            ..Default::default()
        },
    }
}

/// Render each [`ConflictResolution`] as a user-facing warning line, in the
/// order returned by the composition engine. Returns an empty `Vec` when
/// `conflicts` is empty so the caller can take the "no conflicts with
/// current config" branch on `is_empty()`.
///
/// Format pinned to `"  {LABEL} {resource_id} <- {winning_source} ({details})"`
/// — two-space indent, capital label, ASCII left-arrow. Any change to this
/// shape is consumer-visible.
pub(crate) fn format_conflict_preview_lines(
    conflicts: &[cfgd_core::composition::ConflictResolution],
) -> Vec<String> {
    conflicts
        .iter()
        .map(|conflict| {
            format!(
                "  {} {} <- {} ({})",
                conflict.resolution_type.label(),
                conflict.resource_id,
                conflict.winning_source,
                conflict.details
            )
        })
        .collect()
}
