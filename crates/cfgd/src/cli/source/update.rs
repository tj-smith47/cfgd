use super::*;
use crate::cli::output_types::SourceOutcome;
use cfgd_core::output::{Doc, OwnerLabel, Printer, Role};

/// Build the `source update <name>` not-found error. Carries the typed
/// `SourceError::NotFound` in the chain so the exit-code downcast in `main.rs`
/// resolves to ExitCode::NotFound (6); the attached CliErrorMeta drives the
/// stable `{"error":"not_found",...}` payload.
fn source_not_found_error(name: &str) -> anyhow::Error {
    crate::cli::cli_error_ctx(
        cfgd_core::errors::CfgdError::Source(cfgd_core::errors::SourceError::NotFound {
            name: name.to_string(),
        })
        .into(),
        name,
        "not_found",
        format!("Source '{}' not found", name),
        serde_json::json!({}),
    )
}

/// Where one source's rows go.
///
/// A run naming a single source is headed `Update source:team`, the spelling
/// every other single-subject `source` verb uses — so an owner section under
/// that heading would write `source:team` twice, two lines apart. A run over
/// every subscribed source is headed with the plural and needs the owner
/// heading to say which rows belong to which source.
struct SourceRows<'a> {
    printer: &'a Printer,
    section: Option<&'a cfgd_core::output::SectionGuard<'a>>,
}

impl SourceRows<'_> {
    fn status_simple(&self, role: Role, subject: impl Into<String>) {
        match self.section {
            Some(section) => {
                section.status_simple(role, subject);
            }
            None => self.printer.status_simple(role, subject),
        }
    }

    fn status(
        &self,
        role: Role,
        subject: impl Into<String>,
    ) -> cfgd_core::output::status_builder::StatusBuilder<'_> {
        match self.section {
            Some(section) => section.status(role, subject),
            None => self.printer.status(role, subject),
        }
    }

    fn hint(&self, text: impl Into<cfgd_core::output::HintCommands>) {
        match self.section {
            Some(section) => {
                section.hint(text);
            }
            None => self.printer.hint(text),
        }
    }

    fn section(&self, name: impl Into<String>) -> cfgd_core::output::SectionGuard<'_> {
        match self.section {
            Some(section) => section.section(name),
            None => self.printer.section(name),
        }
    }
}

/// The subscription knobs `source update` can set beside its fetch. `None` is
/// "the caller said nothing about this knob", which has to stay distinct from
/// `Some(false)`: a stored `true` must survive an ordinary `cfgd source update`.
#[derive(Default, Clone, Copy)]
pub struct SubscriptionEdits {
    pub require_signed_commits: Option<bool>,
    pub allow_scripts: Option<bool>,
}

impl SubscriptionEdits {
    fn entries(&self) -> Vec<(&'static str, bool)> {
        [
            ("requireSignedCommits", self.require_signed_commits),
            ("allowScripts", self.allow_scripts),
        ]
        .into_iter()
        .filter_map(|(key, value)| value.map(|v| (key, v)))
        .collect()
    }
}

/// Write the asked-for subscription knobs into `cfgd.yaml` and report back what
/// the tree actually holds afterwards.
///
/// The block is MINTED when it is absent, `null`, or a scalar — every
/// `SubscriptionSpec` field is `#[serde(default)]` and the rewrite path prunes
/// an empty mapping, so a source legitimately carries no `subscription:` key at
/// all, and a hand-written `subscription:` with no children parses to `null`.
/// Refusing either shape failed a command that only ever asked to set a value;
/// skipping the insert on either shape was worse, because the caller still
/// announced a write that never happened.
///
/// The return value is READ BACK out of the tree that is about to be written,
/// never echoed from `asked`: the success line and the `-o json` payload may
/// only report a demand the file really records.
fn write_subscription_knobs(
    config_path: &Path,
    name: &str,
    asked: &[(&'static str, bool)],
) -> anyhow::Result<Vec<(&'static str, bool)>> {
    let mut written = Vec::new();
    with_source_config(config_path, name, |source_entry| {
        let map = source_entry
            .as_mapping_mut()
            .ok_or_else(|| anyhow::anyhow!("source '{name}' is not a mapping"))?;
        let key = serde_yaml::Value::String("subscription".into());
        if !map.get(&key).is_some_and(serde_yaml::Value::is_mapping) {
            map.insert(key.clone(), serde_yaml::Value::Mapping(Default::default()));
        }
        let subscription = map
            .get_mut(&key)
            .and_then(serde_yaml::Value::as_mapping_mut)
            .ok_or_else(|| {
                anyhow::anyhow!("source '{name}' subscription block is not a mapping")
            })?;
        for (k, v) in asked {
            subscription.insert(
                serde_yaml::Value::String((*k).into()),
                serde_yaml::Value::Bool(*v),
            );
        }
        written = asked
            .iter()
            .filter_map(|(k, _)| {
                subscription
                    .get(serde_yaml::Value::String((*k).into()))
                    .and_then(serde_yaml::Value::as_bool)
                    .map(|landed| (*k, landed))
            })
            .collect();
        Ok(())
    })?;
    Ok(written)
}

pub fn cmd_source_update(
    cli: &Cli,
    printer: &Printer,
    name: Option<&str>,
    edits: SubscriptionEdits,
) -> anyhow::Result<()> {
    let error_count = run_source_update(cli, printer, name, edits)?;

    // A scripted consumer must be able to detect that a source failed to
    // update from the exit code alone. `run_source_update` already emitted the
    // summary Doc; exit nonzero directly here (mirroring cmd_status's
    // --exit-code path) so the failure isn't re-rendered as a second error
    // line by the central sink. Kept out of the core so the body above stays
    // unit-testable in-process (process::exit would abort the test binary).
    if error_count > 0 {
        cfgd_core::exit::ExitCode::Error.exit();
    }

    Ok(())
}

/// Core of `source update`: fetches each configured source, emits the summary
/// Doc, and returns the number of sources that failed to update.
pub fn run_source_update(
    cli: &Cli,
    printer: &Printer,
    name: Option<&str>,
    edits: SubscriptionEdits,
) -> anyhow::Result<usize> {
    // A run with one named subject is headed the way every other single-subject
    // `source` verb is (`Add source:team`), so the family reads as one family;
    // the plural stays for the form that really does update all of them.
    match name {
        Some(name) => printer.heading_owner_prefixed("Update", &OwnerLabel::new("source", name)),
        None => printer.heading("Update Sources"),
    }

    let config_path = cli.config.clone();
    let mut cfg = config::load_config(&config_path)?;
    drain_config_deprecations(printer, &mut cfg);

    if cfg.spec.sources.is_empty() {
        // A specific source was requested but the config has no sources: that is
        // a NotFound, not a success. Only the update-ALL form (name == None) is
        // an informational no-op here.
        if let Some(name) = name {
            return Err(source_not_found_error(name));
        }
        printer.emit(
            Doc::new()
                .status(Role::Info, "No sources configured")
                .with_data(serde_json::json!({ "sources": [] })),
        );
        return Ok(0);
    }

    let cache_dir = source_cache_dir(cli)?;
    let mut mgr = SourceManager::new(&cache_dir);
    mgr.set_allow_unsigned(cfg.spec.security.as_ref().is_some_and(|s| s.allow_unsigned));
    let state = open_state_store(cli.state_dir.as_deref(), cli.scope())?;

    let sources_to_update: Vec<&config::SourceSpec> = if let Some(name) = name {
        cfg.spec.sources.iter().filter(|s| s.name == name).collect()
    } else {
        cfg.spec.sources.iter().collect()
    };

    if sources_to_update.is_empty()
        && let Some(name) = name
    {
        return Err(source_not_found_error(name));
    }

    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct UpdateEntry {
        name: String,
        status: SourceOutcome,
        commit: Option<String>,
        perm_changes: usize,
    }
    let mut entries: Vec<UpdateEntry> = Vec::new();
    let mut knob_changes = serde_json::Map::new();
    let solo = sources_to_update.len() == 1 && name.is_some();

    for source in &sources_to_update {
        // Whether the fetch landed, held back until the knob rows below have
        // had their say: a bare `√ Updated` beside a row that names the knob it
        // changed is a word the reader already read. A fetch-only run has no
        // knob row, and there the bare row IS the outcome.
        let mut fetch_updated = false;
        let mut knob_rows = 0usize;
        // ONE owner section per source per run: the fetch outcome and the knob
        // rows both belong to it, and opening a second heading for the same
        // source made one run report `source:team` twice. `_or_collapse` so a
        // source that says nothing at all leaves no empty heading behind.
        let owner_sec = (!solo)
            .then(|| printer.section_owner_or_collapse(&OwnerLabel::new("source", &source.name)));
        let source_sec = SourceRows {
            printer,
            section: owner_sec.as_ref(),
        };
        // `load_source` narrates the clone/fetch through `printer.run`, which
        // is a top-level emit: with the owner section open it must render at
        // the section's depth instead of tripping the structural assert.
        let _inherit = printer.depth_inheritance();
        // Capture old manifest before fetching (for permission change detection)
        let source_dir = cache_dir.join(&source.name);
        let old_manifest = if source_dir.exists() {
            mgr.parse_manifest(&source.name, &source_dir).ok()
        } else {
            None
        };

        // The fetch is the wait; the caller words its own failure line just
        // below, so the bar retires silently on both arms.
        let load = printer.narrate_silent(format!("Fetching source:{}", source.name), |_| {
            mgr.load_source(source, printer)
        });
        match load {
            Ok(()) => {
                if let Some(cached) = mgr.get(&source.name) {
                    // Detect permission-expanding changes between old and new manifests
                    let perm_changes = if let Some(ref old) = old_manifest {
                        let old_input = build_permission_input(&source.name, &old.spec.policy);
                        let new_input =
                            build_permission_input(&source.name, &cached.manifest.spec.policy);
                        composition::detect_permission_changes(&[old_input], &[new_input])
                    } else {
                        Vec::new()
                    };

                    // The per-source owner group above binds across both the
                    // prompt and the success emit so the canonical
                    // accept-confirm-then-success line nests under the same
                    // heading as the prompt context bullets. Every line inside
                    // names its outcome only — the group heading says whose.
                    if !perm_changes.is_empty() {
                        let perm_sec = source_sec.section("Permission Changes");
                        for change in &perm_changes {
                            // Shown rather than stripped, the module review
                            // screen's policy: the confirm below approves
                            // exactly the text on this row.
                            perm_sec.status_simple(
                                Role::Warn,
                                cfgd_core::escape_control_chars(&change.description),
                            );
                        }
                    }

                    let proceed = if !perm_changes.is_empty() {
                        match printer.prompt_confirm("Accept permission changes?") {
                            Ok(true) => true,
                            Ok(false) => {
                                source_sec.status_simple(
                                    Role::Info,
                                    "Skipped (permission changes rejected)",
                                );
                                entries.push(UpdateEntry {
                                    name: source.name.clone(),
                                    status: SourceOutcome::Skipped,
                                    commit: cached.last_commit.clone(),
                                    perm_changes: perm_changes.len(),
                                });
                                false
                            }
                            Err(_) => {
                                source_sec.status_simple(Role::Info, "Skipped (prompt cancelled)");
                                entries.push(UpdateEntry {
                                    name: source.name.clone(),
                                    status: SourceOutcome::Cancelled,
                                    commit: cached.last_commit.clone(),
                                    perm_changes: perm_changes.len(),
                                });
                                false
                            }
                        }
                    } else {
                        true
                    };

                    if proceed {
                        state.upsert_config_source(&cfgd_core::state::ConfigSourceUpsert {
                            name: &source.name,
                            origin_url: &source.origin.url,
                            origin_branch: &source.origin.branch,
                            last_commit: cached.last_commit.as_deref(),
                            source_version: cached.manifest.metadata.version.as_deref(),
                            pinned_version: source.sync.pin_version.as_deref(),
                            last_commit_signed: cached.head_signed,
                        })?;

                        // Keep the sources lockfile in sync with the updated commit SHA.
                        if let Some(ref commit) = cached.last_commit {
                            let lock_entry = cfgd_core::config::SourceLockEntry {
                                name: source.name.clone(),
                                url: source.origin.url.clone(),
                                pin_version: source.sync.pin_version.clone(),
                                resolved_ref: cached.resolved_ref.clone(),
                                resolved_commit: commit.clone(),
                                locked_at: cfgd_core::utc_now_iso8601(),
                            };
                            let cfg_dir = config_dir(cli);
                            if let Err(e) =
                                cfgd_core::update_source_lock_entry(&cfg_dir, lock_entry)
                            {
                                source_sec.status_simple(
                                    Role::Warn,
                                    super::sources_lock_update_warning(&e),
                                );
                            }
                        }

                        fetch_updated = true;
                        entries.push(UpdateEntry {
                            name: source.name.clone(),
                            status: SourceOutcome::Updated,
                            commit: cached.last_commit.clone(),
                            perm_changes: perm_changes.len(),
                        });
                    }
                }
            }
            Err(e) => {
                // Under the owner heading, the same shape sync settles on: the
                // row says what failed, the heading says whose it is, and the
                // cause is stated once.
                source_sec
                    .status(Role::Fail, "Update failed")
                    .detail(super::source_failure_detail(&e));
                source_sec.hint(super::source_failure_next_step(&e, &source.name));
                state.update_config_source_status(&source.name, "error")?;
                entries.push(UpdateEntry {
                    name: source.name.clone(),
                    status: SourceOutcome::Error,
                    commit: None,
                    perm_changes: 0,
                });
            }
        }

        // Applied AFTER the fetch, never before: `--require-signed-commits`
        // records a demand on every FUTURE fetch of this source. Enforcing it
        // against the very invocation that sets it would fail the command that
        // only ever asked to write a config value, and hide the refusal under
        // an update error instead of the sync that meets the demand.
        let asked = edits.entries();
        if name.is_some() && !asked.is_empty() {
            let before = (
                source.subscription.require_signed_commits,
                source.subscription.allow_scripts,
            );
            let written = write_subscription_knobs(&config_path, &source.name, &asked)?;
            for (key, value) in &written {
                let old = match *key {
                    "requireSignedCommits" => before.0,
                    _ => before.1,
                };
                source_sec
                    .status(Role::Ok, super::subscription_knob_label(key))
                    .detail(format!(
                        "{} {} {}",
                        cfgd_core::yes_no(Some(old)),
                        printer.arrow(),
                        cfgd_core::yes_no(Some(*value))
                    ));
                knob_rows += 1;
                knob_changes.insert((*key).to_string(), serde_json::Value::Bool(*value));
            }
        }

        if fetch_updated && knob_rows == 0 {
            source_sec.status_simple(Role::Ok, "Updated");
        }
    }

    let updated_count = entries
        .iter()
        .filter(|e| e.status == SourceOutcome::Updated)
        .count();
    let error_count = entries.iter().filter(|e| e.status.refused()).count();
    let skipped_count = entries.iter().filter(|e| e.status.declined()).count();
    let (role, summary) = match (updated_count, error_count, skipped_count) {
        (0, e, _) if e > 0 => (
            Role::Fail,
            format!("{} failed to update", cfgd_core::pluralize(e, "source")),
        ),
        (_, 0, 0) => (
            Role::Ok,
            format!("Updated {}", cfgd_core::pluralize(updated_count, "source")),
        ),
        _ => (
            Role::Warn,
            format!(
                "Updated {}, skipped {}, errored {}",
                updated_count, skipped_count, error_count
            ),
        ),
    };

    let trust_changed = knob_changes.contains_key("requireSignedCommits");
    let knobs_written = !knob_changes.is_empty();
    let mut payload = serde_json::json!({
        "sources": entries,
        "updated": updated_count,
        "skipped": skipped_count,
        "errors": error_count,
    });
    // Additive, and only when this invocation actually edited a knob: an
    // invocation that named none is byte-identical to what it emitted before
    // the flags existed.
    if !knob_changes.is_empty()
        && let Some(obj) = payload.as_object_mut()
    {
        obj.insert(
            "subscription".to_string(),
            serde_json::Value::Object(knob_changes),
        );
    }

    let mut doc = Doc::new().status(role, summary);
    // A clean run that changed something closes on what to do about it; a run
    // that changed nothing, or whose failures already hint per source, does
    // not invite an apply of nothing.
    if role == Role::Ok && (updated_count > 0 || knobs_written) {
        doc = doc.hint(super::success_next_step(super::Mutation::SourceUpdated {
            trust_changed,
        }));
    }
    printer.emit(doc.with_data(&payload));

    Ok(error_count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// The source entry's `subscription:` value is substituted per case, so one
    /// seed covers every shape the block can arrive in.
    fn seed_config(dir: &Path, subscription: &str) -> std::path::PathBuf {
        let path = dir.join("cfgd.yaml");
        std::fs::write(
            &path,
            format!(
                "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n  sources:\n    - name: acme\n      origin:\n        type: Git\n        url: https://example.com/acme/dev.git\n        branch: main\n{subscription}"
            ),
        )
        .expect("write seed config");
        path
    }

    /// Read a knob back out of the file on disk — never out of the value that
    /// was asked for.
    fn knob_on_disk(path: &Path, key: &str) -> Option<bool> {
        let raw: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(path).expect("read config"))
                .expect("parse config");
        raw.get("spec")?
            .get("sources")?
            .get(0)?
            .get("subscription")?
            .get(key)?
            .as_bool()
    }

    /// Every shape the `subscription:` block can be in when the knob is asked
    /// for: absent entirely (the rewrite path prunes an empty mapping), `null`
    /// (hand-written with no children), a scalar (hand-written nonsense), and
    /// an existing mapping. Each must write AND read the value back.
    #[test]
    fn every_subscription_block_shape_is_written_and_read_back() {
        for (case, block) in [
            ("absent", ""),
            ("null", "      subscription:\n"),
            ("scalar", "      subscription: yes-please\n"),
            (
                "mapping",
                "      subscription:\n        requireSignedCommits: false\n",
            ),
        ] {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = seed_config(dir.path(), block);

            let written =
                write_subscription_knobs(&path, "acme", &[("requireSignedCommits", true)])
                    .unwrap_or_else(|e| panic!("{case}: write failed: {e}"));

            assert_eq!(
                written,
                vec![("requireSignedCommits", true)],
                "{case}: the reported write must be what the tree holds"
            );
            assert_eq!(
                knob_on_disk(&path, "requireSignedCommits"),
                Some(true),
                "{case}: the file must record the knob"
            );
        }
    }

    /// A knob the invocation never named is not touched, and one it did name
    /// lands beside it rather than replacing the block.
    #[test]
    fn an_unasked_knob_survives_a_write_of_its_sibling() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = seed_config(
            dir.path(),
            "      subscription:\n        allowScripts: true\n",
        );

        let written = write_subscription_knobs(&path, "acme", &[("requireSignedCommits", true)])
            .expect("write knobs");

        assert_eq!(written, vec![("requireSignedCommits", true)]);
        assert_eq!(knob_on_disk(&path, "allowScripts"), Some(true));
    }

    /// The parsed flags, through the same `paired_flag` mapping the dispatcher
    /// uses, so a rewired flag fails here rather than in a golden.
    fn edits_from_argv(argv: &[&str]) -> SubscriptionEdits {
        let cli = Cli::try_parse_from(argv).expect("parse argv");
        match cli.command {
            Some(crate::cli::Command::Source {
                command:
                    crate::cli::SourceCommand::Update {
                        require_signed_commits,
                        no_require_signed_commits,
                        allow_scripts,
                        no_allow_scripts,
                        ..
                    },
            }) => SubscriptionEdits {
                require_signed_commits: crate::cli::paired_flag(
                    require_signed_commits,
                    no_require_signed_commits,
                ),
                allow_scripts: crate::cli::paired_flag(allow_scripts, no_allow_scripts),
            },
            _ => panic!("expected argv to parse as `source update`: {argv:?}"),
        }
    }

    /// Both halves of both toggle pairs, end to end: the flag as typed decides
    /// the boolean the file records.
    #[test]
    fn each_toggle_flag_writes_its_own_value() {
        for (flag, key, expected) in [
            ("--require-signed-commits", "requireSignedCommits", true),
            ("--no-require-signed-commits", "requireSignedCommits", false),
            ("--allow-scripts", "allowScripts", true),
            ("--no-allow-scripts", "allowScripts", false),
        ] {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = seed_config(dir.path(), "");
            let edits = edits_from_argv(&["cfgd", "source", "update", "acme", flag]);

            let written = write_subscription_knobs(&path, "acme", &edits.entries())
                .unwrap_or_else(|e| panic!("{flag}: write failed: {e}"));

            assert_eq!(written, vec![(key, expected)], "{flag}: reported write");
            assert_eq!(knob_on_disk(&path, key), Some(expected), "{flag}: on disk");
        }
    }

    /// An invocation naming no toggle writes nothing at all, so an ordinary
    /// `cfgd source update` cannot clear a stored demand.
    #[test]
    fn an_invocation_naming_no_toggle_asks_for_no_write() {
        let edits = edits_from_argv(&["cfgd", "source", "update", "acme"]);
        assert!(edits.entries().is_empty());
    }

    /// A pair's two halves are mutually exclusive, and every toggle needs the
    /// source it edits.
    #[test]
    fn contradictory_and_nameless_toggles_are_refused() {
        for argv in [
            vec![
                "cfgd",
                "source",
                "update",
                "acme",
                "--require-signed-commits",
                "--no-require-signed-commits",
            ],
            vec![
                "cfgd",
                "source",
                "update",
                "acme",
                "--allow-scripts",
                "--no-allow-scripts",
            ],
            vec!["cfgd", "source", "update", "--require-signed-commits"],
            vec!["cfgd", "source", "update", "--no-require-signed-commits"],
            vec!["cfgd", "source", "update", "--allow-scripts"],
            vec!["cfgd", "source", "update", "--no-allow-scripts"],
        ] {
            assert!(
                Cli::try_parse_from(&argv).is_err(),
                "must be refused: {argv:?}"
            );
        }
    }
}
