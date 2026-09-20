//! What a config document does not declare that the running binary's schema
//! carries.

use std::path::Path;

use cfgd_core::config::CfgdConfig;
use cfgd_core::output::{Doc, Printer, Role};
use cfgd_core::state::StateStore;
use cfgd_schema::MigrationPolicy;

use crate::cli::helpers::no_config_error;
use crate::cli::{Cli, Mutation, mutate_config_yaml, success_next_step};

/// Dotted key paths the typed value carries that the document on disk does
/// not name.
pub(in crate::cli) struct PendingAlignment {
    /// Sorted, e.g. `["spec.fileStrategy", "spec.migrationPolicy"]`.
    pub keys: Vec<String>,
}

/// What the document at `on_disk` does not declare that the typed value `cfg`
/// carries. `cfg` is the parse of those same bytes, so the two sides differ by
/// exactly what the deserializer materialized.
///
/// Nothing here reports a VERSION. A document at another `apiVersion` cannot
/// reach this function: `validate_api_version` refuses every version the
/// conversion table does not name, and the shipped table names one, so a `cfg`
/// that parsed spells `API_VERSION`. The release that adds a second row to
/// that table is the one that can report a version here, with a document that
/// can populate it.
pub(in crate::cli) fn pending_alignment(cfg: &CfgdConfig, on_disk: &str) -> PendingAlignment {
    let declared = serde_yaml::from_str(on_disk).unwrap_or(serde_yaml::Value::Null);
    PendingAlignment {
        keys: crate::cli::helpers::undeclared_scalar_keys(cfg, &declared),
    }
}

/// The parsed config and the bytes it was parsed from — the two halves
/// [`pending_alignment`] compares. `None` when the file is absent,
/// unreadable or does not parse: a gate that cannot read the document has
/// nothing to say about it, and the command boundary below reports the load
/// failure on its own.
fn read_pair(config_path: &Path) -> Option<(CfgdConfig, String)> {
    let on_disk = std::fs::read_to_string(config_path).ok()?;
    let cfg = cfgd_core::config::parse_config(&on_disk, config_path).ok()?;
    Some((cfg, on_disk))
}

/// Materialize every pending key with the value the typed config already
/// carries. The ONE writer this feature has: `--write`, the `Update` policy
/// and an accepted prompt all reach it, so no two of them can write
/// differently. `mutate_config_yaml` re-prepends the leading comment block
/// and re-parses the result before it replaces the file.
///
/// The two path walkers are `cli::config_cmd`'s own: the mutable one creates
/// the intermediate mappings a key under an absent section needs, which is
/// what `cfgd config set` already relies on.
fn write_alignment(
    config_path: &Path,
    cfg: &CfgdConfig,
    pending: &PendingAlignment,
) -> anyhow::Result<()> {
    let materialized = serde_yaml::to_value(cfg)?;
    mutate_config_yaml(config_path, true, |raw| {
        for key in &pending.keys {
            // Every pending key was read off `materialized` in the first
            // place, so a miss here is not a state this can reach; skipping
            // is still the right answer to one, since the alternative is
            // failing a write over a key nobody asked for.
            let Ok(value) = crate::cli::config_cmd::walk_yaml_path(&materialized, key) else {
                continue;
            };
            let value = value.clone();
            let (parent, leaf) = crate::cli::config_cmd::walk_yaml_path_mut(raw, key)?;
            parent.insert(serde_yaml::Value::String(leaf), value);
        }
        Ok(())
    })
}

/// What a reader is told about a document behind the schema: what is
/// missing, and the command that settles it.
fn warn_message(pending: &PendingAlignment) -> String {
    format!(
        "This config does not declare {} this build reads ({}); `cfgd config migrate` reports them, `cfgd config migrate --write` materializes them",
        cfgd_core::pluralize(pending.keys.len(), "field"),
        pending.keys.join(", ")
    )
}

/// The question itself. It names the same fields the warning does, because a
/// reader answering yes is agreeing to a write of exactly those keys — and
/// the keys are what the answer is recorded against.
fn prompt_message(pending: &PendingAlignment) -> String {
    format!(
        "Add {} this build reads to your config ({})?",
        cfgd_core::pluralize(pending.keys.len(), "field"),
        pending.keys.join(", ")
    )
}

/// The gate's own write: the same [`write_alignment`] the verb calls, with the
/// failure reported rather than dropped. This runs before dispatch, so there
/// is no command for the error to fail — but a reader who said yes and got
/// silence would believe the file had been written.
fn align(printer: &Printer, config_path: &Path, cfg: &CfgdConfig, pending: &PendingAlignment) {
    if let Err(e) = write_alignment(config_path, cfg, pending) {
        printer.alert(format!("Could not update this config: {e}"));
    }
}

/// What the store already holds for this question, with a refused read
/// REPORTED. A store that cannot answer has answered nothing, so the reader
/// is asked again — and told why, because a silent failure is being asked
/// every run with nothing on screen to fix.
fn recorded_answer(
    printer: &Printer,
    store: &StateStore,
    config_path: &Path,
    api_version: &str,
    keys: &[String],
) -> Option<bool> {
    match store.migration_answer(config_path, api_version, keys) {
        Ok(answer) => answer,
        Err(e) => {
            printer.alert(format!("Could not read config_migrations: {e}"));
            None
        }
    }
}

/// Hold the answer just given, with a refused write REPORTED: silence there
/// throws the answer away and asks the same question on the next run.
fn record_answer(
    printer: &Printer,
    store: &StateStore,
    config_path: &Path,
    api_version: &str,
    accepted: bool,
    keys: &[String],
) {
    if let Err(e) = store.record_migration_answer(config_path, api_version, accepted, keys) {
        printer.alert(format!("Could not record config_migrations: {e}"));
    }
}

/// Report what this build's schema carries that the config document does not
/// declare, and under `write` materialize it.
pub fn cmd_config_migrate(cli: &Cli, printer: &Printer, write: bool) -> anyhow::Result<()> {
    let config_path = &cli.config;
    let Some((cfg, on_disk)) = read_pair(config_path) else {
        return Err(no_config_error(printer, config_path));
    };
    let pending = pending_alignment(&cfg, &on_disk);
    let wrote = write && !pending.keys.is_empty();
    if wrote {
        write_alignment(config_path, &cfg, &pending)?;
    }

    // Each row is the key and the value the write would materialize, read
    // off the same typed value the write reads, so the report and the write
    // cannot name two things.
    let materialized = serde_yaml::to_value(&cfg).unwrap_or(serde_yaml::Value::Null);
    let rows: Vec<(String, String)> = pending
        .keys
        .iter()
        .map(|key| {
            let value = crate::cli::config_cmd::walk_yaml_path(&materialized, key)
                .ok()
                .and_then(|v| serde_yaml::to_string(v).ok())
                .map_or_else(|| cfgd_core::ABSENT.to_string(), |s| s.trim().to_string());
            (key.clone(), value)
        })
        .collect();

    let (role, subject) = match (pending.keys.is_empty(), wrote) {
        (true, _) => (
            Role::Ok,
            // verdict-row-ok: a state verdict about the document, not an act cfgd performed
            "Config declares every field this build reads".to_string(),
        ),
        (false, true) => (
            Role::Ok,
            format!("Wrote {}", cfgd_core::pluralize(rows.len(), "field")),
        ),
        (false, false) => (
            Role::Warn,
            format!(
                "{} not declared by this config",
                cfgd_core::pluralize(rows.len(), "field")
            ),
        ),
    };

    let mut doc = Doc::new().status(role, subject).kv_block(rows);
    if wrote {
        // The alignment changes what the composition reads, so the settled
        // write closes where every composition edit closes.
        doc = doc.hint(success_next_step(Mutation::ConfigMigrated));
    } else if !pending.keys.is_empty() {
        doc = doc.hint("Materialize them with `cfgd config migrate --write`");
    }
    printer.emit(doc.with_data(serde_json::json!({
        "path": cfgd_core::to_posix_string(config_path),
        "pendingKeys": pending.keys,
        "written": wrote,
    })));
    Ok(())
}

/// The load-time gate: what a document behind this build's schema earns
/// before dispatch, per `spec.migrationPolicy`.
///
/// The state store is opened HERE, and only in the `Prompt` arm, because that
/// is the one arm with an answer to remember: opening it at the call site
/// would create a state root for `cfgd paths`, `cfgd explain` and every other
/// read that records nothing.
pub fn gate_on_load(printer: &Printer, cli: &Cli, policy: MigrationPolicy, assume_yes: bool) {
    if policy == MigrationPolicy::Ignore {
        return;
    }
    let config_path = &cli.config;
    let Some((cfg, on_disk)) = read_pair(config_path) else {
        return;
    };
    let pending = pending_alignment(&cfg, &on_disk);
    if pending.keys.is_empty() {
        return;
    }
    // `--yes` takes the prompt, so the recorded answer is consulted only
    // where a prompt would actually be shown.
    let effective = match policy {
        MigrationPolicy::Prompt if assume_yes => MigrationPolicy::Update,
        MigrationPolicy::Prompt if !printer.can_prompt() => MigrationPolicy::Warn,
        other => other,
    };
    match effective {
        MigrationPolicy::Update => align(printer, config_path, &cfg, &pending),
        MigrationPolicy::Warn => printer.alert(warn_message(&pending)),
        MigrationPolicy::Prompt => {
            let store =
                match crate::cli::registry::open_state_store(cli.state_dir.as_deref(), cli.scope())
                {
                    Ok(store) => store,
                    Err(e) => {
                        printer.alert(format!("Could not open the state store: {e}"));
                        return;
                    }
                };
            if recorded_answer(
                printer,
                &store,
                config_path,
                &cfg.api_version,
                &pending.keys,
            )
            .is_some()
            {
                return;
            }
            let accepted = printer
                .prompt_confirm(&prompt_message(&pending))
                .unwrap_or(false);
            if accepted {
                align(printer, config_path, &cfg, &pending);
            }
            // Recorded whichever way it was answered: a "no" is an answer,
            // and asking it again every run is how a knob nobody wants
            // becomes a knob nobody can escape.
            record_answer(
                printer,
                &store,
                config_path,
                &cfg.api_version,
                accepted,
                &pending.keys,
            );
        }
        MigrationPolicy::Ignore => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `Cli` pointed at a fixture config, parsed from an argv so every
    /// global default is the real one. A hand-written `Cli { … }` literal goes
    /// stale the moment a global flag is added, and this part adds one.
    /// `state_dir` is supplied the same way, so nothing a gate does can reach
    /// the invoking user's own state root.
    fn cli_with_config(config: &std::path::Path, state_dir: Option<&std::path::Path>) -> Cli {
        use clap::Parser;
        let config = config.to_string_lossy().into_owned();
        let mut argv = vec!["cfgd".to_string(), "--config".to_string(), config];
        if let Some(dir) = state_dir {
            argv.push("--state-dir".to_string());
            argv.push(dir.to_string_lossy().into_owned());
        }
        argv.push("config".to_string());
        argv.push("migrate".to_string());
        Cli::try_parse_from(argv).expect("the fixture argv parses")
    }

    /// `--write` materializes the missing keys through the config crate's own
    /// write path, so the leading comment block and the schema modeline
    /// survive and the result re-parses. Without `--write` the file is
    /// byte-identical.
    #[test]
    fn config_migrate_writes_only_under_write_and_keeps_the_modeline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfgd.yaml");
        let doc = "# yaml-language-server: $schema=https://example.invalid/Config.json\n\
                   apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: t\nspec:\n  profile: work\n";
        std::fs::write(&path, doc).unwrap();
        let cli = cli_with_config(&path, None);
        let printer = cfgd_core::test_helpers::test_printer();

        cmd_config_migrate(&cli, &printer, false).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            doc,
            "a report writes nothing"
        );

        cmd_config_migrate(&cli, &printer, true).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(
            after.starts_with("# yaml-language-server:"),
            "the modeline survived: {after}"
        );
        assert!(
            after.contains("migrationPolicy: Prompt"),
            "the missing key was materialized: {after}"
        );
        let reparsed =
            cfgd_core::config::parse_config(&after, &path).expect("the written document re-parses");
        assert!(
            pending_alignment(&reparsed, &after).keys.is_empty(),
            "a second run has nothing left to do"
        );
    }

    /// A non-interactive `Prompt` degrades to `Warn`: the reader is told, the
    /// file is untouched, and NOTHING is recorded — recording an unasked
    /// question would answer it forever.
    #[test]
    fn a_non_tty_prompt_warns_and_records_no_answer() {
        use cfgd_core::output::{Printer, Verbosity};
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfgd.yaml");
        let doc =
            "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: t\nspec: {}\n";
        std::fs::write(&path, doc).unwrap();
        let cli = cli_with_config(&path, Some(state.path()));
        // A test capture is never a terminal, so `can_prompt()` is false here
        // for the same reason it is false in a pipeline — the degrade under
        // test.
        let (printer, _stdout, stderr) = Printer::for_test_split_streams(Verbosity::Normal);

        gate_on_load(&printer, &cli, MigrationPolicy::Prompt, false);
        printer.flush();

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            doc,
            "a warned document is untouched"
        );
        let text = cfgd_core::test_helpers::captured_text(&stderr);
        assert!(
            text.contains("cfgd config migrate"),
            "the reader is told what settles it: {text}"
        );
        let store = StateStore::open_in_dir(state.path()).unwrap();
        assert_eq!(
            store
                .migration_answer(
                    &path,
                    cfgd_core::API_VERSION,
                    &["spec.migrationPolicy".to_string()]
                )
                .unwrap(),
            None,
            "an unasked question is not recorded"
        );
    }

    /// Which arms of the gate write, over the whole policy vocabulary: only
    /// `Update` — and a `Prompt` the caller already answered with `--yes`,
    /// which IS that answer — puts anything on disk. `Warn` and `Ignore` reach
    /// no writer at all, and `Ignore` says nothing either.
    #[test]
    fn only_the_answering_arms_of_the_gate_write_the_alignment() {
        use cfgd_core::output::{Printer, Verbosity};
        let doc =
            "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: t\nspec: {}\n";
        let written = |policy, assume_yes| {
            let dir = tempfile::tempdir().unwrap();
            let state = tempfile::tempdir().unwrap();
            let path = dir.path().join("cfgd.yaml");
            std::fs::write(&path, doc).unwrap();
            let cli = cli_with_config(&path, Some(state.path()));
            let (printer, _stdout, stderr) = Printer::for_test_split_streams(Verbosity::Normal);
            gate_on_load(&printer, &cli, policy, assume_yes);
            printer.flush();
            let after = std::fs::read_to_string(&path).unwrap();
            (
                after != doc,
                cfgd_core::test_helpers::captured_text(&stderr),
            )
        };
        assert!(written(MigrationPolicy::Update, false).0, "Update writes");
        assert!(
            written(MigrationPolicy::Prompt, true).0,
            "`--yes` takes the prompt, and taking it is answering yes"
        );
        assert!(!written(MigrationPolicy::Warn, false).0, "Warn writes not");
        let (ignored_wrote, ignored_said) = written(MigrationPolicy::Ignore, false);
        assert!(!ignored_wrote, "Ignore writes not");
        assert!(
            ignored_said.trim().is_empty(),
            "Ignore says nothing either: {ignored_said}"
        );
    }

    /// A state store that cannot answer the migration question says so. The
    /// reader is asked again either way, and a silent read failure means being
    /// asked every run with nothing on screen to fix; a silent write failure
    /// means the answer just given is thrown away.
    #[test]
    fn a_store_that_cannot_hold_the_answer_reports_instead_of_swallowing() {
        use cfgd_core::output::{Printer, Verbosity};
        let state = tempfile::tempdir().unwrap();
        let store = StateStore::open_in_dir(state.path()).unwrap();
        store.drop_config_migrations_table().unwrap();
        let path = state.path().join("cfgd.yaml");
        let keys = vec!["spec.migrationPolicy".to_string()];

        let (printer, _stdout, stderr) = Printer::for_test_split_streams(Verbosity::Normal);
        assert_eq!(
            recorded_answer(&printer, &store, &path, cfgd_core::API_VERSION, &keys),
            None,
            "a store that cannot be read has answered nothing"
        );
        record_answer(&printer, &store, &path, cfgd_core::API_VERSION, true, &keys);
        printer.flush();

        let text = cfgd_core::test_helpers::captured_text(&stderr);
        assert!(
            text.contains("Could not read config_migrations")
                && text.contains("Could not record config_migrations"),
            "both the read and the write name the store and what it said: {text}"
        );
        assert_eq!(
            text.matches("no such table").count(),
            2,
            "each half carries what the store said, not a wording of its own: {text}"
        );
    }

    /// A document that names no `migrationPolicy` is behind the schema by
    /// exactly that key; one that names it is behind by nothing. The detection
    /// reads the SERIALIZED typed value against the declared tree, so a field
    /// added to `ConfigSpec` later appears here with no edit to this function.
    #[test]
    fn a_document_missing_a_defaulted_field_is_reported_as_behind_by_that_key() {
        let doc = "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: t\nspec:\n  profile: work\n";
        let cfg = cfgd_core::config::parse_config(doc, std::path::Path::new("cfgd.yaml")).unwrap();
        let pending = pending_alignment(&cfg, doc);
        assert!(
            pending.keys.contains(&"spec.migrationPolicy".to_string()),
            "the document names no migrationPolicy: {:?}",
            pending.keys
        );
        assert!(
            !pending.keys.contains(&"spec.profile".to_string()),
            "a key the document declares is never pending: {:?}",
            pending.keys
        );

        let aligned = format!("{doc}  migrationPolicy: Prompt\n");
        let cfg =
            cfgd_core::config::parse_config(&aligned, std::path::Path::new("cfgd.yaml")).unwrap();
        assert!(
            !pending_alignment(&cfg, &aligned)
                .keys
                .contains(&"spec.migrationPolicy".to_string()),
            "a declared key is not pending"
        );
    }

    /// `cfgd init` scaffolds the theme as the union's scalar arm
    /// (`theme: dracula`), which IS `theme: {name: dracula}`. A document
    /// written that way declares the name beneath it, so nothing under
    /// `spec.output.theme` is behind the schema, and nothing is reported that
    /// `cfgd config set` would then refuse to write.
    #[test]
    fn a_theme_written_as_the_unions_scalar_arm_declares_the_name_beneath_it() {
        let doc = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  output:\n    theme: dracula\n  fileStrategy: Symlink\n";
        let cfg = cfgd_core::config::parse_config(doc, std::path::Path::new("cfgd.yaml")).unwrap();
        let pending = pending_alignment(&cfg, doc);
        assert!(
            !pending
                .keys
                .iter()
                .any(|key| key.starts_with("spec.output.theme")),
            "the scalar arm declares the theme: {:?}",
            pending.keys
        );
        assert!(
            pending.keys.contains(&"spec.migrationPolicy".to_string()),
            "a key the document really is missing is still reported: {:?}",
            pending.keys
        );
    }

    /// An element of a declared list is data, and neither key walker
    /// addresses a sequence, so no reported key steps through an index.
    #[test]
    fn no_reported_key_names_an_element_of_a_declared_list() {
        let doc = "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: t\nspec:\n  origin:\n    - type: Git\n      url: git@github.com:me/dotfiles.git\n";
        let cfg = cfgd_core::config::parse_config(doc, std::path::Path::new("cfgd.yaml")).unwrap();
        let pending = pending_alignment(&cfg, doc);
        assert!(
            !pending.keys.iter().any(|key| key
                .split('.')
                .any(|segment| segment.parse::<usize>().is_ok())),
            "no reported key steps through an index: {:?}",
            pending.keys
        );
        assert!(
            !pending
                .keys
                .iter()
                .any(|key| key.starts_with("spec.origin.")),
            "what a declared element carries is data, not a pending key: {:?}",
            pending.keys
        );
        assert!(
            pending.keys.contains(&"spec.migrationPolicy".to_string()),
            "the walk still reports the keys it should: {:?}",
            pending.keys
        );
    }

    /// Every key reported is one `cfgd config migrate` can materialize: each
    /// is written into the document through the same walker that verb uses,
    /// and the re-parsed document is behind the schema by nothing. A key the
    /// writer refuses, or one whose write the reader cannot see afterwards,
    /// leaves the second call non-empty. The fixture is what `cfgd init`
    /// scaffolds, so the union's scalar arm is on the path the writer has to
    /// descend.
    #[test]
    fn every_key_pending_alignment_reports_is_one_the_config_writer_can_set() {
        let doc = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  output:\n    theme: dracula\n  fileStrategy: Symlink\n  profile: work\n";
        let cfg = cfgd_core::config::parse_config(doc, std::path::Path::new("cfgd.yaml")).unwrap();
        let pending = pending_alignment(&cfg, doc);
        assert!(
            pending.keys.contains(&"spec.migrationPolicy".to_string()),
            "the fixture reports something to write: {:?}",
            pending.keys
        );

        let serialized = serde_yaml::to_value(&cfg).unwrap();
        let mut written: serde_yaml::Value = serde_yaml::from_str(doc).unwrap();
        for key in &pending.keys {
            let value = crate::cli::config_cmd::walk_yaml_path(&serialized, key)
                .unwrap_or_else(|e| panic!("the read walker resolves '{key}': {e}"))
                .clone();
            let (parent, leaf) = crate::cli::config_cmd::walk_yaml_path_mut(&mut written, key)
                .unwrap_or_else(|e| panic!("the write walker sets '{key}': {e}"));
            parent.insert(serde_yaml::Value::String(leaf), value);
        }

        let written = serde_yaml::to_string(&written).unwrap();
        let cfg = cfgd_core::config::parse_config(&written, std::path::Path::new("cfgd.yaml"))
            .unwrap_or_else(|e| panic!("the written document parses: {e}"));
        let pending = pending_alignment(&cfg, &written);
        assert!(
            pending.keys.is_empty(),
            "a document carrying every reported key is behind by nothing: {:?}",
            pending.keys
        );
    }
}
