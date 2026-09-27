use super::*;
use cfgd_core::PathDisplayExt;
use cfgd_core::config::validate_source_priority;
use cfgd_core::output::{OwnerLabel, Role, SectionBuilder};

// --- Source cache layout ---

pub(crate) fn source_cache_dir(cli: &Cli) -> anyhow::Result<std::path::PathBuf> {
    Ok(cfgd_core::resolve_cache_dir(cli.cache_dir.as_deref(), cli.scope())?.join("sources"))
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
        allow_scripts: false,
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
    let n = input.parse::<u32>().map_err(|_| {
        crate::cli::invalid_argument(
            "--priority",
            input,
            format!("invalid priority: '{}' (must be a number)", input),
        )
    })?;
    checked_priority(n, "--priority")
}

/// A subscription priority the config parser will hold, refused as the
/// argument `flag` it came from, spelled as that command's `--help` prints it,
/// when it is out of range.
pub(crate) fn checked_priority(n: u32, flag: &str) -> anyhow::Result<u32> {
    validate_source_priority(n).map_err(|m| crate::cli::invalid_argument(flag, &n.to_string(), m))
}

/// The `subscription` block of the source entry `name`, as the mapping a verb
/// editing one of its knobs writes into.
///
/// An absent block and a bare `subscription:` are an empty one, the rule
/// [`section_mapping_mut`](crate::cli::config_cmd::section_mapping_mut) applies
/// to every section of the config document; any other shape is refused naming
/// what the entry holds there, so no knob is reported written into a block
/// that could not take it.
pub(super) fn subscription_mapping_mut<'a>(
    entry: &'a mut serde_yaml::Value,
    config_path: &Path,
    name: &str,
) -> anyhow::Result<&'a mut serde_yaml::Mapping> {
    use crate::cli::config_cmd::{
        SHAPE_MAPPING, blocking_shape, section_mapping_mut, section_shape_refusal,
    };
    let at = format!("sources[{name}]");
    let found = blocking_shape(entry);
    let entry = section_mapping_mut(entry)
        .ok_or_else(|| section_shape_refusal(config_path, &at, found, SHAPE_MAPPING))?;
    let block = entry
        .entry(serde_yaml::Value::String("subscription".into()))
        .or_insert(serde_yaml::Value::Null);
    let found = blocking_shape(block);
    section_mapping_mut(block).ok_or_else(|| {
        section_shape_refusal(config_path, &subscription_path(name), found, SHAPE_MAPPING)
    })
}

/// How a refusal names the `subscription` block of the source entry `name`.
pub(super) fn subscription_path(name: &str) -> String {
    format!("sources[{name}].subscription")
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

/// Append a per-source breakdown of pending decisions to a [`SectionBuilder`].
///
/// Grouped by source name (BTreeMap → alphabetical order). Each source becomes
/// a nested subsection headed by its `source:<name>` owner token, whose first
/// status line carries the count and whose remaining lines say what each item
/// would put on the machine. Returns the augmented builder so callers can
/// chain further composition.
///
/// The single renderer behind both `cfgd decide`'s listing and `cfgd status`'s
/// Pending Decisions section: the same rows under two headings would let one
/// screen's grammar drift from the other's.
///
/// Rows only: the instruction for answering them is the caller's closing hint
/// (`cfgd_core::reconciler::answer_decisions_hint`), left-aligned at the foot
/// of the surface like every other closing hint cfgd prints. Rendered from
/// inside the section it wore the group's indent and had no blank line above
/// it, so the one hint in the product that looked like a row read as part of
/// the list it was instructing the reader to act on.
pub(crate) fn build_pending_decisions_table_section(
    s: SectionBuilder,
    decisions: &[cfgd_core::state::PendingDecision],
    contents: &cfgd_core::reconciler::DecisionContents,
) -> SectionBuilder {
    cfgd_core::reconciler::decisions_by_source(decisions)
        .into_iter()
        .fold(s, |s, (source_name, items)| {
            // The same `source:<name>` token every other source-owned line
            // carries — one screen must not name one source two ways.
            s.subsection_owner(&OwnerLabel::new("source", source_name), |sub| {
                items.iter().fold(sub, |sub, item| {
                    let (subject, detail) = contents.decision_row(item);
                    sub.status_with(Role::Info, subject, |f| match detail {
                        Some(detail) => f.detail(detail),
                        None => f,
                    })
                })
            })
        })
}

pub(crate) fn add_source_to_config(
    config_path: &Path,
    source: &config::SourceSpec,
) -> anyhow::Result<()> {
    if !config_path.exists() {
        return Err(crate::cli::cli_error(
            cfgd_core::to_posix_string(config_path),
            "no_config",
            format!("Config file not found: {}", config_path.posix()),
            serde_json::json!({ "path": cfgd_core::to_posix_string(config_path) }),
        ));
    }

    mutate_config_yaml(config_path, |raw| {
        use crate::cli::config_cmd;
        let sources = config_cmd::spec_mapping_mut(raw, config_path)?
            .entry(serde_yaml::Value::String("sources".into()))
            .or_insert(serde_yaml::Value::Null);
        let found = config_cmd::blocking_shape(sources);
        let seq = config_cmd::section_sequence_mut(sources).ok_or_else(|| {
            config_cmd::section_shape_refusal(
                config_path,
                "sources",
                found,
                config_cmd::SHAPE_SEQUENCE,
            )
        })?;
        let source_value = serde_yaml::to_value(source)?;
        seq.push(source_value);
        Ok(())
    })?;
    Ok(())
}

pub(crate) fn remove_source_from_config(config_path: &Path, name: &str) -> anyhow::Result<()> {
    if !config_path.exists() {
        return Ok(());
    }
    mutate_config_yaml(config_path, |raw| {
        if let Some(spec) = raw.get_mut("spec")
            && let Some(sources) = spec.get_mut("sources")
            // section-write-ok: a remover; an absent list holds no entry to remove
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
    })?;
    Ok(())
}

fn find_source_in_config<'a>(
    raw: &'a mut serde_yaml::Value,
    source_name: &str,
) -> Option<&'a mut serde_yaml::Value> {
    raw.get_mut("spec")?
        .get_mut("sources")?
        // section-write-ok: finds an existing entry, and an absent list holds none
        .as_sequence_mut()?
        .iter_mut()
        .find(|item| {
            item.get("name")
                .and_then(|n| n.as_str())
                .map(|n| n == source_name)
                .unwrap_or(false)
        })
}

/// What one write of the config document left behind.
#[derive(Debug)]
pub(crate) struct ConfigWrite {
    /// The typed config the written document parses to.
    pub config: config::CfgdConfig,
    /// The dotted keys the alignment wrote at the value the parse gave them,
    /// e.g. `spec.daemon.reconcile.autoApply` put back after an unset.
    pub filled: Vec<String>,
}

/// The config document at `path` as the tree every writer edits, read in the
/// format its extension names: a `.toml` document is parsed as TOML, the way
/// `parse_config` reads it, and every other document as YAML.
pub(in crate::cli) fn config_tree(
    contents: &str,
    path: &Path,
) -> anyhow::Result<serde_yaml::Value> {
    if is_toml_document(path) {
        let table: toml::Table = toml::from_str(contents)?;
        Ok(serde_yaml::to_value(table)?)
    } else {
        Ok(serde_yaml::from_str(contents)?)
    }
}

/// `tree` serialized in the format [`config_tree`] read it in.
///
/// TOML has no null, so a tree holding one (`config set <key> "~"`) cannot
/// be written as TOML at all. That is refused `parse_failed` naming the key,
/// the refusal the same write earns on a YAML document from the parser; a
/// dropped null would turn the write into a silent no-op.
fn render_config_tree(tree: &serde_yaml::Value, path: &Path) -> anyhow::Result<String> {
    if !is_toml_document(path) {
        return Ok(serde_yaml::to_string(tree)?);
    }
    toml::to_string(tree).map_err(|e| {
        let reason = match first_null_key(tree, &mut Vec::new()) {
            Some(key) => format!("{key} is null, and TOML has no null value"),
            None => e.to_string(),
        };
        crate::cli::cli_error(
            cfgd_core::to_posix_string(path),
            "parse_failed",
            format!("config would become invalid: {reason}"),
            serde_json::json!({
                "path": cfgd_core::to_posix_string(path),
                "reason": reason,
            }),
        )
    })
}

/// The dotted key of the first null in `tree`, in document order.
fn first_null_key(tree: &serde_yaml::Value, at: &mut Vec<String>) -> Option<String> {
    match tree {
        serde_yaml::Value::Null => Some(at.join(".")),
        serde_yaml::Value::Mapping(map) => map.iter().find_map(|(k, v)| {
            at.push(k.as_str().map_or_else(|| format!("{k:?}"), str::to_string));
            let found = first_null_key(v, at);
            at.pop();
            found
        }),
        serde_yaml::Value::Sequence(items) => items.iter().enumerate().find_map(|(i, v)| {
            at.push(i.to_string());
            let found = first_null_key(v, at);
            at.pop();
            found
        }),
        _ => None,
    }
}

fn is_toml_document(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("toml")
}

/// The one read-parse-mutate-write loop for the config document, in the
/// format its extension names.
///
/// Loads the document at `config_path`, hands the mutable root
/// `serde_yaml::Value` to `f`, parses the result as a `Config`, aligns what
/// the closure made pending, and atomically writes it with the file's leading
/// comment block re-prepended. A closure that leaves a document the parser
/// refuses writes nothing: the refusal is `parse_failed`, naming the parser's
/// reason.
///
/// The alignment keeps the load-time migration gate asking only its own
/// question. A present section declares every scalar this build reads under
/// it, so a write that brings a section into existence (`config set
/// daemon.reconcile.autoApply` on a document with no `daemon`) would
/// otherwise leave it partial, and an unset inside a present section would
/// leave that section missing the key it removed. Each such key is written at
/// the value the parse gave it. A key the document was already missing
/// before the write, under a section it already held, is the gate's to ask
/// about under `spec.migrationPolicy`, so the write leaves it alone.
///
/// Use this for every write of the config document; the open-coded
/// `read_to_string → from_str → mutate → to_string → atomic_write_str`
/// pattern is how writers once diverged on validation.
pub(crate) fn mutate_config_yaml<F>(config_path: &Path, f: F) -> anyhow::Result<ConfigWrite>
where
    F: FnOnce(&mut serde_yaml::Value) -> anyhow::Result<()>,
{
    if !config_path.is_file() {
        return Err(
            cfgd_core::errors::CfgdError::from(cfgd_core::errors::ConfigError::NotFound {
                path: config_path.to_path_buf(),
            })
            .into(),
        );
    }
    let contents = std::fs::read_to_string(config_path)?;
    let mut raw = config_tree(&contents, config_path)?;
    let before = raw.clone();
    f(&mut raw)?;
    let mut body = render_config_tree(&raw, config_path)?;
    let cfg = config::parse_config(&body, config_path).map_err(|e| {
        crate::cli::cli_error(
            cfgd_core::to_posix_string(config_path),
            "parse_failed",
            format!("config would become invalid: {}", e),
            serde_json::json!({
                "path": cfgd_core::to_posix_string(config_path),
                "reason": e.to_string(),
            }),
        )
    })?;
    let filled = align_what_the_write_left_pending(&mut raw, &before, &cfg)?;
    if !filled.is_empty() {
        body = render_config_tree(&raw, config_path)?;
    }
    let output = cfgd_core::config::with_leading_comments(&contents, &body);
    // Pre-flight the config dir for real write access so a read-only dir surfaces
    // the typed TargetNotWritable naming the path; the atomic write below would
    // only report a bare `Permission denied (os error 13)`.
    if let Some(parent) = config_path.parent()
        && parent.exists()
        && matches!(
            cfgd_core::probe_dir_writable(parent),
            cfgd_core::DirWritable::NotWritable
        )
    {
        return Err(cfgd_core::errors::CfgdError::File(
            cfgd_core::errors::FileError::TargetNotWritable {
                path: config_path.to_path_buf(),
            },
        )
        .into());
    }
    cfgd_core::atomic_write_str(config_path, &output)?;
    Ok(ConfigWrite {
        config: cfg,
        filled,
    })
}

/// Materialize into `raw` every scalar `cfg` carries that `raw` does not
/// declare AND that this write made pending, with the value `cfg` carries.
/// `cfg` is the parse of `raw`, so the two differ by exactly what the
/// deserializer defaulted. Returns the keys written.
///
/// The keys are the ones `config_schema::pending_alignment` reports, read by
/// the same traversal over the tree already in hand, then narrowed by
/// [`made_pending_by_the_write`] against `before`, the document as it stood
/// ahead of the closure. The mutable walker creates the intermediate mappings
/// a key under an absent section needs, the way `cfgd config set` relies on
/// it.
fn align_what_the_write_left_pending(
    raw: &mut serde_yaml::Value,
    before: &serde_yaml::Value,
    cfg: &config::CfgdConfig,
) -> anyhow::Result<Vec<String>> {
    let (keys, materialized) =
        crate::cli::helpers::undeclared_scalar_keys_in(serde_yaml::to_value(cfg)?, raw);
    // The document as the closure left it: a key filled below creates the
    // mappings above it, which must not read as sections this write created
    // when the next key is judged.
    let written = raw.clone();
    let mut filled = Vec::new();
    for key in keys {
        if !made_pending_by_the_write(before, &written, &key) {
            continue;
        }
        // Every key was read off `materialized` in the first place, so a miss
        // is a state this cannot reach; skipping it keeps a write the reader
        // asked for from failing over a key nobody asked for.
        let Ok(value) = crate::cli::config_cmd::walk_yaml_path(&materialized, &key) else {
            continue;
        };
        let (parent, leaf) = crate::cli::config_cmd::walk_yaml_path_mut(raw, &key)?;
        parent.insert(serde_yaml::Value::String(leaf), value.clone());
        filled.push(key);
    }
    Ok(filled)
}

/// Whether a key the written document does not declare is one this write
/// made pending: the document declared it before the write (the write
/// removed it), or a section above it was absent, null or empty before the
/// write and holds something in `written`, the tree the closure left (the
/// write created that section). Every other undeclared key was already
/// missing from a section the document held, or sits under a section the
/// write did not touch, which is the question the load-time gate asks under
/// `spec.migrationPolicy`.
fn made_pending_by_the_write(
    before: &serde_yaml::Value,
    written: &serde_yaml::Value,
    key: &str,
) -> bool {
    let declared = match key.strip_prefix("spec.") {
        // `walk_yaml_path` reads a scalar union arm relative to `spec`.
        Some(rest) => before
            .get("spec")
            .is_some_and(|spec| crate::cli::config_cmd::walk_yaml_path(spec, rest).is_ok()),
        None => crate::cli::config_cmd::walk_yaml_path(before, key).is_ok(),
    };
    if declared {
        return true;
    }
    let created = |now: Option<&serde_yaml::Value>| matches!(now, Some(serde_yaml::Value::Mapping(map)) if !map.is_empty());
    let segments: Vec<&str> = key.split('.').collect();
    let mut node = before;
    let mut now = Some(written);
    for segment in &segments[..segments.len() - 1] {
        let now_below = now.and_then(|n| n.get(segment));
        match node.get(segment) {
            None | Some(serde_yaml::Value::Null) => return created(now_below),
            Some(serde_yaml::Value::Mapping(map)) if map.is_empty() => {
                return created(now_below);
            }
            Some(section @ serde_yaml::Value::Mapping(_)) => {
                node = section;
                now = now_below;
            }
            // A scalar union arm stands for its mapping, which is present.
            Some(_) => return false,
        }
    }
    false
}

/// Load config YAML, find a named source, apply a mutation, and write back.
/// The closure receives the mutable source entry; the helper handles I/O,
/// and the write is refused and aligned by [`mutate_config_yaml`] like every
/// other write of the document.
pub(super) fn with_source_config<F>(
    config_path: &Path,
    source_name: &str,
    f: F,
) -> anyhow::Result<()>
where
    F: FnOnce(&mut serde_yaml::Value) -> anyhow::Result<()>,
{
    mutate_config_yaml(config_path, |raw| {
        let source = find_source_in_config(raw, source_name).ok_or_else(|| {
            crate::cli::cli_error(
                source_name,
                "not_found",
                format!("source '{}' not found in config file", source_name),
                serde_json::json!({ "path": cfgd_core::to_posix_string(config_path) }),
            )
        })?;
        f(source)
    })?;
    Ok(())
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
        allow_scripts: false,
    }
}

/// Render each [`cfgd_core::composition::ConflictResolution`] as a user-facing warning line, in the
/// order returned by the composition engine. Returns an empty `Vec` when
/// `conflicts` is empty so the caller can take the "no conflicts with
/// current config" branch on `is_empty()`.
///
/// Format pinned to `"{resource_id}: {details}"` — no leading indent (the
/// caller renders each line through `status_simple` inside a section, which
/// supplies its own). Any change to this shape is consumer-visible.
///
/// `conflict.details` is `composition::record`'s persisted string and
/// keeps its own `<-` shape in storage (see that module's doc comment); this
/// is a DISPLAY path only, so the arrow is reworded to "from" here through
/// `crate::cli::helpers::reword_conflict_arrow_for_display` — the same
/// display-side reword `display_and_persist_conflicts` applies to the
/// primary `apply`/`plan` surface, so the two never disagree — rather than
/// touching what gets written to `source_conflicts.detail`. The
/// wrapper used to also restate `resolution_type.label()`,
/// `conflict.resource_id` and `conflict.winning_source` ahead of
/// `details` — but `details` already carries the label, the resource and
/// the source (`record.rs` is the only producer), so on every real conflict
/// the two halves said the same thing in two different shapes
/// (`"LOCKED package:apt:curl from acme (LOCKED curl <- acme)"`). Dropping
/// the restatement to `resource_id: details` keeps the resource's
/// kind-prefixed name (useful for scanning a list of mixed kinds) without
/// repeating the relationship a second time.
pub(crate) fn format_conflict_preview_lines(
    conflicts: &[cfgd_core::composition::ConflictResolution],
) -> Vec<String> {
    conflicts
        .iter()
        .map(|conflict| {
            format!(
                "{}: {}",
                conflict.resource_id,
                crate::cli::helpers::reword_conflict_arrow_for_display(&conflict.details)
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cfgd_core::config::MAX_SOURCE_PRIORITY;
    use cfgd_core::output::OutputFormat;

    fn cli_with_dirs(cache_dir: Option<PathBuf>, state_dir: Option<PathBuf>) -> Cli {
        Cli {
            config: PathBuf::from("cfgd.yaml"),
            config_explicit: false,
            profile: None,
            verbose: 0,
            quiet: true,
            no_color: true,
            color: crate::cli::ColorWhen::Auto,
            output: OutputFormatArg(OutputFormat::Table),
            list_envelope: false,
            hints: false,
            no_hints: false,
            theme: None,
            mask_env_values: None,
            migration_policy: None,
            jsonpath: None,
            yes: false,
            state_dir,
            config_dir: None,
            cache_dir,
            runtime_dir: None,
            scope_arg: crate::cli::ScopeArg::User,
            command: None,
        }
    }

    #[test]
    fn source_cache_dir_follows_cache_override_not_state_dir() {
        let cli = cli_with_dirs(
            Some(PathBuf::from("/over/cache")),
            Some(PathBuf::from("/over/state")),
        );
        let dir = source_cache_dir(&cli).unwrap();
        assert_eq!(dir, PathBuf::from("/over/cache").join("sources"));
        assert!(
            !dir.starts_with("/over/state"),
            "source cache must not follow --state-dir, got: {}",
            dir.display()
        );
    }

    #[test]
    fn parse_priority_input_rejects_over_cap() {
        // u32::MAX is over MAX_SOURCE_PRIORITY — must error.
        let result = parse_priority_input("4294967295");
        assert!(result.is_err(), "u32::MAX must be rejected");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("exceeds maximum"),
            "error should mention 'exceeds maximum': {msg}"
        );
    }

    #[test]
    fn parse_priority_input_accepts_at_cap() {
        let cap = MAX_SOURCE_PRIORITY.to_string();
        let result = parse_priority_input(&cap);
        assert!(
            result.is_ok(),
            "MAX_SOURCE_PRIORITY must be accepted, got: {:?}",
            result
        );
        assert_eq!(result.unwrap(), MAX_SOURCE_PRIORITY);
    }

    #[test]
    fn parse_priority_input_rejects_non_numeric() {
        let result = parse_priority_input("abc");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("must be a number"));
    }

    #[test]
    fn parse_priority_input_accepts_typical_value() {
        let result = parse_priority_input("500");
        assert_eq!(result.unwrap(), 500);
    }

    // An edit inside a source entry is a write of the whole document, so a
    // value the parser refuses there is refused the way `config set` refuses
    // one, and the file keeps the bytes it had.
    #[test]
    fn a_source_entry_edit_the_parser_refuses_is_refused_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfgd.yaml");
        let doc = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  sources:\n    - name: acme\n      origin:\n        type: Git\n        url: https://example.com/acme.git\n";
        std::fs::write(&path, doc).unwrap();

        let err = with_source_config(&path, "acme", |entry| {
            entry["subscription"] = serde_yaml::Value::String("not a mapping".into());
            Ok(())
        })
        .expect_err("the parser refuses a scalar subscription");

        let meta = err
            .downcast_ref::<crate::cli::CliErrorMeta>()
            .expect("the refusal carries its kind");
        assert_eq!(meta.error_kind, "parse_failed");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), doc);
    }
}
