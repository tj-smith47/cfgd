//! What a config document does not declare that the running binary's schema
//! carries.

use std::path::Path;

use cfgd_core::config::CfgdConfig;
use cfgd_core::output::{Doc, Printer, Role};
use cfgd_core::state::StateStore;
use cfgd_schema::MigrationPolicy;

use crate::cli::helpers::no_config_error;
use crate::cli::{Cli, Command, ConfigCommand, Mutation, mutate_config_yaml, success_next_step};

/// Dotted key paths the typed value carries that the document on disk does
/// not name.
pub(in crate::cli) struct PendingAlignment {
    /// Sorted, e.g. `["spec.fileStrategy", "spec.migrationPolicy"]`.
    pub keys: Vec<String>,
}

/// What the document at `on_disk` does not declare that the typed value `cfg`
/// carries. `cfg` is the parse of those same bytes, so the two sides differ by
/// exactly what the deserializer materialized. `path` names the format the
/// bytes are read in, the way `parse_config` reads it.
///
/// Nothing here reports a VERSION. A document at another `apiVersion` cannot
/// reach this function: `validate_api_version` refuses every version the
/// conversion table does not name, and the shipped table names one, so a `cfg`
/// that parsed spells `API_VERSION`. The release that adds a second row to
/// that table is the one that can report a version here, with a document that
/// can populate it.
pub(in crate::cli) fn pending_alignment(
    cfg: &CfgdConfig,
    on_disk: &str,
    path: &Path,
) -> PendingAlignment {
    let declared =
        crate::cli::source::config_tree(on_disk, path).unwrap_or(serde_yaml::Value::Null);
    PendingAlignment {
        keys: crate::cli::helpers::undeclared_scalar_keys(cfg, &declared),
    }
}

/// The `spec`-relative key `spec.migrationPolicy` is addressed by.
const MIGRATION_POLICY_KEY: &str = "migrationPolicy";

/// Whether a `config set` / `config unset` key names the knob the gate itself
/// runs under. The key is resolved the way the setter resolves it — the
/// optional `spec.` prefix folded away, then the nested-key resolution both
/// verbs perform — so the exemption and the write cannot disagree about which
/// field is being edited.
fn names_migration_policy(key: &str) -> bool {
    let key = crate::cli::config_cmd::spec_relative_key(key);
    crate::cli::config_cmd::nested_output_key(key)
        .as_deref()
        .unwrap_or(key)
        == MIGRATION_POLICY_KEY
}

/// Why the load-time gate is withheld from this invocation, and `None` for
/// every invocation it runs for.
///
/// The gate is withheld from an invocation whose own subject IS the migration
/// question, and from nothing else: the verb that remediates it, the two
/// setters replacing the knob it runs under, and the hand edit it would write
/// underneath. Every other verb is a reader the advisory is written for, or
/// has no document for the gate to read.
///
/// `cfgd init` is withheld here because it runs the gate itself, against
/// the document it wrote, once that document exists: at load time there is
/// usually nothing on disk yet, and the path it writes need not be the one
/// `--config` names.
///
/// The reasons are documentation — nothing renders them — and the match names
/// every `config` subcommand, so one added later fails to compile until it is
/// classified either way.
pub fn gate_exempt(command: Option<&Command>) -> Option<&'static str> {
    let command = match command {
        Some(Command::Init { .. }) => {
            return Some(
                "the verb writes the config, and runs the gate against the document it wrote",
            );
        }
        Some(Command::Config { command }) => command,
        _ => return None,
    };
    match command {
        ConfigCommand::Migrate { .. } => Some(
            "the verb is the gate's own remediation, and a write ahead of its report makes the report lie",
        ),
        ConfigCommand::Set { key, .. } | ConfigCommand::Unset { key }
            if names_migration_policy(key) =>
        {
            Some("the reader is replacing the knob the gate runs under")
        }
        ConfigCommand::Edit => Some(
            "the reader is about to open the bytes by hand, and a prompt ahead of $EDITOR rewrites the file underneath it",
        ),
        ConfigCommand::Show
        | ConfigCommand::Get { .. }
        | ConfigCommand::Set { .. }
        | ConfigCommand::Unset { .. } => None,
    }
}

/// What one invocation brings to the load-time gate: the policy it named on
/// its own, whether it answered yes up front, whether it is the daemon, and
/// the state root an answer is recorded under. Every caller builds it with
/// [`GateInvocation::of`], so the load-time call and `cfgd init`'s cannot read
/// those four facts two ways.
#[derive(Clone, Copy)]
pub struct GateInvocation<'a> {
    /// `--migration-policy` / `CFGD_MIGRATION_POLICY`, unfolded.
    pub policy_override: Option<MigrationPolicy>,
    /// `--yes` / `CFGD_YES`.
    pub assume_yes: bool,
    pub is_daemon: bool,
    /// `--state-dir`, where a prompt's answer is held.
    pub state_dir: Option<&'a Path>,
    pub scope: cfgd_core::Scope,
}

impl<'a> GateInvocation<'a> {
    /// The invocation `cli` parsed to.
    pub fn of(cli: &'a Cli, is_daemon: bool) -> Self {
        Self {
            policy_override: crate::cli::migration_policy_override(cli.migration_policy.as_deref()),
            assume_yes: cli.yes,
            is_daemon,
            state_dir: cli.state_dir.as_deref(),
            scope: cli.scope(),
        }
    }
}

/// The policy a run answers to once the daemon is accounted for.
///
/// A daemon never blocks on a prompt and never rewrites a file something else
/// tracks, so both arms that would write fold to a report; `Warn` and
/// `Ignore` pass through, and off the daemon nothing folds at all. The fold
/// lives here because the reconcile loop is in `cfgd-core` and cannot call
/// into this crate, and [`gate_on_load`] is its one caller: the override and
/// the stored policy fold at the same site, so the two halves of one decision
/// cannot be taken in two places.
pub fn daemon_folded_policy(is_daemon: bool, policy: MigrationPolicy) -> MigrationPolicy {
    match (is_daemon, policy) {
        (true, MigrationPolicy::Prompt | MigrationPolicy::Update) => MigrationPolicy::Warn,
        (_, policy) => policy,
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
/// differently. The keys are written inside the closure: the alignment
/// `mutate_config_yaml` performs covers only what a write itself made
/// pending, and these keys were missing before this write began.
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
    mutate_config_yaml(config_path, |raw| {
        for key in &pending.keys {
            // Every pending key was read off `materialized` in the first
            // place, so a miss here is not a state this can reach; skipping
            // is still the right answer to one, since the alternative is
            // failing a write over a key nobody asked for.
            let Ok(value) = crate::cli::config_cmd::walk_yaml_path(&materialized, key) else {
                continue;
            };
            let (parent, leaf) = crate::cli::config_cmd::walk_yaml_path_mut(raw, key)?;
            parent.insert(serde_yaml::Value::String(leaf), value.clone());
        }
        Ok(())
    })?;
    Ok(())
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

/// The gate's own write: the same [`write_alignment`] the verb calls, with
/// both outcomes reported. A written file earns an `Ok` row naming the fields
/// added, the way `cfgd config migrate --write` reports its write. A failed
/// write is an alert: this runs before dispatch, so there is no command for
/// the error to fail, and a reader who said yes and got silence would believe
/// the file had been written.
fn align(printer: &Printer, config_path: &Path, cfg: &CfgdConfig, pending: &PendingAlignment) {
    match write_alignment(config_path, cfg, pending) {
        Ok(()) => printer.status_simple(
            Role::Ok,
            format!(
                "Added {} this build reads to your config ({})",
                cfgd_core::pluralize(pending.keys.len(), "field"),
                pending.keys.join(", ")
            ),
        ),
        Err(e) => printer.alert(format!("Could not update this config: {e}")),
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
            printer.alert(format!("Could not read the recorded migration answer: {e}"));
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
        printer.alert(format!("Could not record the migration answer: {e}"));
    }
}

/// The recorded answer, read from a state store that ALREADY exists.
///
/// A state root nothing has written to holds no answer, so none is created to
/// ask it: a read-only verb on a machine that has never applied leaves the
/// disk exactly as it found it. The store comes back with the answer so the
/// caller can hold a new one in the same handle; `None` for the whole pair is
/// a state directory that could not be resolved or opened, already reported.
fn consult_recorded(
    printer: &Printer,
    invocation: &GateInvocation<'_>,
    config_path: &Path,
    api_version: &str,
    keys: &[String],
) -> Option<(Option<StateStore>, Option<bool>)> {
    let dir = match crate::cli::helpers::run_state_dir(invocation.state_dir, invocation.scope) {
        Ok(dir) => dir,
        Err(e) => {
            printer.alert(format!("Could not resolve the state directory: {e}"));
            return None;
        }
    };
    if !dir.join(cfgd_core::state::STATE_DB_FILENAME).exists() {
        return Some((None, None));
    }
    let store = open_store(printer, invocation)?;
    let answer = recorded_answer(printer, &store, config_path, api_version, keys);
    Some((Some(store), answer))
}

/// The state store, opened to HOLD something. Every failure is reported: a
/// reader who answered and got silence would believe the answer was kept.
fn open_store(printer: &Printer, invocation: &GateInvocation<'_>) -> Option<StateStore> {
    match crate::cli::registry::open_state_store(invocation.state_dir, invocation.scope) {
        Ok(store) => Some(store),
        Err(e) => {
            printer.alert(format!("Could not open the state store: {e}"));
            None
        }
    }
}

/// Report what this build's schema carries that the config document does not
/// declare, and under `write` materialize it.
pub fn cmd_config_migrate(cli: &Cli, printer: &Printer, write: bool) -> anyhow::Result<()> {
    let config_path = &cli.config;
    let Some((cfg, on_disk)) = read_pair(config_path) else {
        return Err(no_config_error(printer, config_path));
    };
    let pending = pending_alignment(&cfg, &on_disk, config_path);
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

/// The load-time gate: what the document at `config_path`, behind this
/// build's schema, earns per `spec.migrationPolicy`. It runs before dispatch
/// against `--config`, and inside `cfgd init` against the document init
/// wrote.
///
/// `invocation.policy_override` is what this invocation said on its own
/// (`--migration-policy` / `CFGD_MIGRATION_POLICY`), unfolded: the daemon
/// fold is taken here, over the override and the stored policy alike, because
/// the stored half never passes through the caller at all. With nothing
/// overridden the stored policy is read
/// off the parse below rather than a second load of the same file, so a gate
/// costs one read of the document whatever it decides — and an overridden
/// `Ignore` costs none at all.
///
/// The state store is opened HERE rather than at the call site, and only
/// where there is an answer to hold: `cfgd paths`, `cfgd explain` and every
/// other read that records nothing leave the state root as they found it.
pub fn gate_on_load(printer: &Printer, invocation: &GateInvocation<'_>, config_path: &Path) {
    let GateInvocation {
        policy_override,
        assume_yes,
        is_daemon,
        ..
    } = *invocation;
    let overridden = policy_override.map(|policy| daemon_folded_policy(is_daemon, policy));
    if overridden == Some(MigrationPolicy::Ignore) {
        return;
    }
    let Some((cfg, on_disk)) = read_pair(config_path) else {
        return;
    };
    let policy =
        overridden.unwrap_or_else(|| daemon_folded_policy(is_daemon, cfg.spec.migration_policy));
    if policy == MigrationPolicy::Ignore {
        return;
    }
    let pending = pending_alignment(&cfg, &on_disk, config_path);
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
            let Some((held, recorded)) = consult_recorded(
                printer,
                invocation,
                config_path,
                &cfg.api_version,
                &pending.keys,
            ) else {
                return;
            };
            if recorded.is_some() {
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
            // becomes a knob nobody can escape. The store is the one the read
            // held, or one opened now that there is something to put in it.
            let store = match held {
                Some(store) => Some(store),
                None => open_store(printer, invocation),
            };
            if let Some(store) = store {
                record_answer(
                    printer,
                    &store,
                    config_path,
                    &cfg.api_version,
                    accepted,
                    &pending.keys,
                );
            }
        }
        MigrationPolicy::Ignore => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::HermeticParse;

    /// A `Cli` pointed at a fixture config, parsed from an argv so every
    /// global default is the real one. A hand-written `Cli { … }` literal goes
    /// stale the moment a global flag is added, and this part adds one.
    /// `state_dir` is supplied the same way, so nothing a gate does can reach
    /// the invoking user's own state root.
    fn cli_with_config(config: &std::path::Path, state_dir: Option<&std::path::Path>) -> Cli {
        cli_running(config, state_dir, &["config", "migrate"])
    }

    /// The same fixture for an argv naming some other verb, so a test about
    /// which invocations the gate runs for can build each one the way clap
    /// would.
    fn cli_running(
        config: &std::path::Path,
        state_dir: Option<&std::path::Path>,
        verb: &[&str],
    ) -> Cli {
        let config = config.to_string_lossy().into_owned();
        let mut argv = vec!["cfgd".to_string(), "--config".to_string(), config];
        if let Some(dir) = state_dir {
            argv.push("--state-dir".to_string());
            argv.push(dir.to_string_lossy().into_owned());
        }
        argv.extend(verb.iter().map(|part| (*part).to_string()));
        Cli::try_parse_hermetic(argv).expect("the fixture argv parses")
    }

    /// The gate as `cli` would run it, with the policy and the up-front yes
    /// the case under test names in place of what the argv parsed to.
    fn gate(
        printer: &Printer,
        cli: &Cli,
        policy_override: Option<MigrationPolicy>,
        assume_yes: bool,
    ) {
        let invocation = GateInvocation {
            policy_override,
            assume_yes,
            ..GateInvocation::of(cli, false)
        };
        gate_on_load(printer, &invocation, &cli.config);
    }

    /// Whether a shell line writes a `cfgd.yaml`: a heredoc naming it, a
    /// redirect into it, or a `tee` of it. A comment line mentioning the file
    /// writes nothing.
    fn writes_a_cfgd_yaml(line: &str) -> bool {
        let code = line.trim_start();
        if code.starts_with('#') {
            return false;
        }
        let Some(file_at) = code.find("cfgd.yaml") else {
            return false;
        };
        code.contains("<<") || code[..file_at].contains('>') || code.contains("tee ")
    }

    /// The word a heredoc opened on `line` ends at, and whether it was opened
    /// with `<<-`, which lets the closing line carry leading tabs.
    fn heredoc_terminator(line: &str) -> Option<(String, bool)> {
        let after = &line[line.find("<<")? + 2..];
        let (after, strip_tabs) = match after.strip_prefix('-') {
            Some(rest) => (rest, true),
            None => (after, false),
        };
        let word: String = after
            .trim_start()
            .trim_start_matches(['\'', '"'])
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        (!word.is_empty()).then_some((word, strip_tabs))
    }

    /// Every `cfgd.yaml` a demo setup script writes is aligned to this build,
    /// so a recorded tape never stops on the migration prompt. The fixtures
    /// are the heredoc bodies the scripts `cat` into a `cfgd.yaml`; a `${VAR}`
    /// the shell would expand is replaced by a URL, the one shape the
    /// fixtures interpolate. A field added to the schema fails this until the
    /// fixtures declare it. Each script's heredocs are counted against every
    /// line that writes a `cfgd.yaml`, so a fixture written in a shape the walk
    /// cannot read fails here too.
    #[test]
    fn every_demo_fixture_config_declares_every_field_this_build_reads() {
        let scripts = cfgd_core::test_helpers::workspace_root().join("demo/scripts");
        let mut setups: Vec<_> = std::fs::read_dir(&scripts)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("setup-") && name.ends_with(".sh"))
            })
            .collect();
        setups.sort();

        let mut fixtures = Vec::new();
        for script in &setups {
            let text = std::fs::read_to_string(script).unwrap();
            let lines: Vec<&str> = text.lines().collect();
            let writes = lines.iter().filter(|line| writes_a_cfgd_yaml(line)).count();
            let mut read = 0;
            let mut at = 0;
            while at < lines.len() {
                let line = lines[at];
                at += 1;
                if !(writes_a_cfgd_yaml(line) && line.contains("<<")) {
                    continue;
                }
                let (terminator, strip_tabs) = heredoc_terminator(line).unwrap_or_else(|| {
                    panic!(
                        "{}:{at}: no heredoc terminator on `{line}`",
                        script.display()
                    )
                });
                let opened = at;
                let mut body = Vec::new();
                loop {
                    let Some(next) = lines.get(at) else {
                        panic!(
                            "{}:{opened}: the heredoc's `{terminator}` is never reached",
                            script.display()
                        );
                    };
                    at += 1;
                    let closing = if strip_tabs {
                        next.trim_start_matches('\t')
                    } else {
                        next
                    };
                    if closing == terminator {
                        break;
                    }
                    body.push(*next);
                }
                let mut doc = String::new();
                let mut rest = body.join("\n");
                while let Some(open) = rest.find("${") {
                    let close = open + rest[open..].find('}').expect("a `${` closes");
                    doc.push_str(&rest[..open]);
                    doc.push_str("http://demo.invalid:8080");
                    rest = rest[close + 1..].to_string();
                }
                doc.push_str(&rest);
                doc.push('\n');
                fixtures.push((format!("{}:{opened}", script.display()), doc));
                read += 1;
            }
            assert_eq!(
                read,
                writes,
                "{}: {writes} lines write a cfgd.yaml and the walk read {read} of them as heredocs",
                script.display()
            );
        }
        assert!(
            fixtures.len() >= 2,
            "the walk found {} cfgd.yaml fixtures under {}",
            fixtures.len(),
            scripts.display()
        );

        for (at, doc) in &fixtures {
            let path = std::path::Path::new("cfgd.yaml");
            let cfg = cfgd_core::config::parse_config(doc, path)
                .unwrap_or_else(|e| panic!("{at}: the fixture parses: {e}"));
            let pending = pending_alignment(&cfg, doc, Path::new("cfgd.yaml"));
            assert!(
                pending.keys.is_empty(),
                "{at}: the fixture leaves {:?} for the migration gate to ask about",
                pending.keys
            );
        }
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
            pending_alignment(&reparsed, &after, Path::new("cfgd.yaml"))
                .keys
                .is_empty(),
            "a second run has nothing left to do"
        );
    }

    /// `cfgd config migrate --write` against a document declaring ONLY the
    /// legacy `spec.theme` must not add `spec.output.theme` beside it. A
    /// document declaring only one spelling must never be told on reload
    /// that `spec.theme` and `spec.output.theme` are both set; that advisory
    /// traces to exactly this write path materializing the fold-derived
    /// nested key onto a document `pending_alignment` should have reported
    /// nothing new for. Proven end to end: load, write, reload — no
    /// advisory, no duplicated key.
    #[test]
    fn migrate_write_does_not_duplicate_a_legacy_theme_into_the_nested_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfgd.yaml");
        let doc = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  theme: dracula\n";
        std::fs::write(&path, doc).unwrap();
        let cli = cli_with_config(&path, None);
        let printer = cfgd_core::test_helpers::test_printer();

        cmd_config_migrate(&cli, &printer, true).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(
            !after.contains("output:") || !after.contains("theme:\n"),
            "the write must not add a nested output.theme block: {after}"
        );

        let reloaded =
            cfgd_core::config::parse_config(&after, &path).expect("the written document re-parses");
        assert!(
            !reloaded.deprecations.iter().any(|d| d.contains("both set")),
            "a document that only ever declared one spelling must not be told they conflict: {:?}",
            reloaded.deprecations
        );
        assert_eq!(
            reloaded.spec.theme().map(|t| t.name.clone()),
            Some("dracula".to_string())
        );
    }

    /// Every legacy output key survives a migrate write without a folded twin.
    ///
    /// `migrate_write_does_not_duplicate_a_legacy_theme_into_the_nested_key`
    /// above exercises `spec.theme` alone. This walks every member of
    /// `LEGACY_OUTPUT_KEYS` (`spec.theme` and `spec.usageHints` today), so a
    /// third legacy key added later must supply a fixture value here before
    /// this test can pass, rather than riding on `spec.theme`'s coverage.
    #[test]
    fn migrate_write_does_not_duplicate_any_legacy_output_key_into_its_nested_key() {
        for (old, new) in cfgd_core::config::LEGACY_OUTPUT_KEYS {
            let leaf = old
                .strip_prefix("spec.")
                .expect("legacy key is spec-relative");
            let value = match leaf {
                "theme" => "dracula",
                "usageHints" => "true",
                other => panic!(
                    "LEGACY_OUTPUT_KEYS gained {other} with no fixture value in this walk; add one"
                ),
            };
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("cfgd.yaml");
            let doc = format!(
                "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  {leaf}: {value}\n"
            );
            std::fs::write(&path, &doc).unwrap();
            let cli = cli_with_config(&path, None);
            let printer = cfgd_core::test_helpers::test_printer();

            cmd_config_migrate(&cli, &printer, true).unwrap();
            let after = std::fs::read_to_string(&path).unwrap();

            let reloaded = cfgd_core::config::parse_config(&after, &path)
                .unwrap_or_else(|e| panic!("{old}: the written document must re-parse: {e}"));
            assert!(
                !reloaded
                    .deprecations
                    .iter()
                    .any(|d| d.contains("are both set")),
                "{old}: a document declaring only one spelling must not be told {old} and {new} conflict: {:?}",
                reloaded.deprecations
            );
            assert!(
                pending_alignment(&reloaded, &after, Path::new("cfgd.yaml"))
                    .keys
                    .is_empty(),
                "{old}: a second run must have nothing left to do"
            );
        }
    }

    /// `source add`'s own write path leaves no folded twin either.
    ///
    /// `source add` appends to `spec.sources` through `add_source_to_config`,
    /// and the alignment every write of the document performs then
    /// materializes what the parse carries that the document does not declare.
    /// That alignment is where a folded twin would most plausibly appear, so
    /// the pin calls the real writer with a real `SourceSpec`: a document
    /// declaring only the legacy `spec.theme` still reloads with no "both set"
    /// advisory and nothing pending.
    #[test]
    fn a_source_add_write_does_not_materialize_a_legacy_output_keys_folded_twin() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfgd.yaml");
        let doc = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  theme: dracula\n";
        std::fs::write(&path, doc).unwrap();

        let source = cfgd_core::config::SourceSpec {
            name: "acme".to_string(),
            origin: serde_yaml::from_str("type: Git\nurl: https://example.com/x.git\n").unwrap(),
            subscription: Default::default(),
            sync: Default::default(),
        };
        crate::cli::add_source_to_config(&path, &source).expect("`source add`'s writer succeeds");

        let after = std::fs::read_to_string(&path).unwrap();
        let reloaded = cfgd_core::config::parse_config(&after, &path)
            .expect("the document source add wrote must re-parse");
        assert_eq!(
            reloaded.spec.sources.len(),
            1,
            "the writer must have appended the entry it was handed"
        );
        assert!(
            !reloaded
                .deprecations
                .iter()
                .any(|d| d.contains("are both set")),
            "a source add must not fold spec.theme into spec.output.theme: {:?}",
            reloaded.deprecations
        );
        assert!(
            !pending_alignment(&reloaded, &after, Path::new("cfgd.yaml"))
                .keys
                .iter()
                .any(|k| k == "spec.output.theme"),
            "a source add must not leave spec.output.theme pending as a folded twin"
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

        gate(&printer, &cli, Some(MigrationPolicy::Prompt), false);
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
        assert!(
            !state
                .path()
                .join(cfgd_core::state::STATE_DB_FILENAME)
                .exists(),
            "an unasked question records nothing, so no state root is created to hold it"
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
            let (printer, stdout, stderr) = Printer::for_test_split_streams(Verbosity::Normal);
            gate(&printer, &cli, Some(policy), assume_yes);
            printer.flush();
            let after = std::fs::read_to_string(&path).unwrap();
            (
                after != doc,
                format!(
                    "{}{}",
                    cfgd_core::test_helpers::captured_text(&stdout),
                    cfgd_core::test_helpers::captured_text(&stderr)
                ),
                state
                    .path()
                    .join(cfgd_core::state::STATE_DB_FILENAME)
                    .exists(),
            )
        };
        let theme = cfgd_core::output::Theme::default();
        let (update_wrote, update_said, _) = written(MigrationPolicy::Update, false);
        assert!(update_wrote, "Update writes");
        let (yes_wrote, yes_said, yes_stored) = written(MigrationPolicy::Prompt, true);
        assert!(
            yes_wrote,
            "`--yes` takes the prompt, and taking it is answering yes"
        );
        // A write the reader asked for is a success row, the way
        // `cfgd config migrate --write` reports the same write.
        for said in [&update_said, &yes_said] {
            let row = said
                .lines()
                .find(|line| line.contains("Added 2 fields this build reads to your config"))
                .unwrap_or_else(|| panic!("the write names what it added: {said}"));
            assert!(
                row.contains(&theme.icon_ok),
                "the write is an Ok row: {row}"
            );
            assert!(
                !said.contains(&theme.icon_warn),
                "nothing about a completed write is a warning: {said}"
            );
        }
        assert!(
            !yes_stored,
            "nobody was asked, so nothing was answered and no state root was created to hold it"
        );
        assert!(!written(MigrationPolicy::Warn, false).0, "Warn writes not");
        let (ignored_wrote, ignored_said, ignored_stored) = written(MigrationPolicy::Ignore, false);
        assert!(!ignored_wrote, "Ignore writes not");
        assert!(
            ignored_said.trim().is_empty(),
            "Ignore says nothing either: {ignored_said}"
        );
        assert!(!ignored_stored, "Ignore opens no store either");
    }

    /// A `cfgd.toml` is the same document in the other format `--config`
    /// reads, so the gate reports it behind by the keys it does not declare
    /// and an `Update` writes the alignment back as TOML.
    #[test]
    fn a_toml_document_is_gated_and_aligned_in_its_own_format() {
        use cfgd_core::output::{Printer, Verbosity};
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfgd.toml");
        let doc = "apiVersion = \"cfgd.io/v1alpha1\"\nkind = \"Config\"\n\n[metadata]\nname = \"t\"\n\n[spec]\nprofile = \"work\"\n";
        std::fs::write(&path, doc).unwrap();
        let cfg = cfgd_core::config::parse_config(doc, &path).unwrap();
        let mut behind = pending_alignment(&cfg, doc, &path).keys;
        behind.sort();
        assert_eq!(
            behind,
            ["spec.fileStrategy", "spec.migrationPolicy"],
            "the TOML document is behind by exactly the keys it does not declare"
        );

        let cli = cli_with_config(&path, Some(state.path()));
        let (printer, _stdout, _stderr) = Printer::for_test_split_streams(Verbosity::Normal);
        gate(&printer, &cli, Some(MigrationPolicy::Update), false);
        printer.flush();

        let written = std::fs::read_to_string(&path).unwrap();
        let table: toml::Table = written
            .parse()
            .unwrap_or_else(|e| panic!("the aligned document is still TOML: {e}\n{written}"));
        assert_eq!(
            table["spec"]["migrationPolicy"].as_str(),
            Some("Prompt"),
            "the alignment declares the policy: {written}"
        );
        let reparsed = cfgd_core::config::parse_config(&written, &path).unwrap();
        assert!(
            pending_alignment(&reparsed, &written, &path)
                .keys
                .is_empty(),
            "nothing is left behind: {written}"
        );
    }

    /// The document every gate fixture below is pointed at: it declares no
    /// `migrationPolicy`, so it is behind the schema by exactly that key.
    const BEHIND_DOC: &str =
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: t\nspec: {}\n";

    /// A capture holding a scripted answer, at the verbosity an alert renders
    /// at. `can_prompt()` is true while the queue holds something, which is
    /// what puts the gate on its real `Prompt` path instead of the non-TTY
    /// degrade.
    fn prompting_printer(answers: Vec<cfgd_core::output::PromptAnswer>) -> (Printer, PromptBuffer) {
        use cfgd_core::output::Verbosity;
        Printer::for_test_with_prompt_responses_at(answers, Verbosity::Normal)
    }

    type PromptBuffer = std::sync::Arc<std::sync::Mutex<String>>;

    /// The answer a reader gives the prompt is acted on AND remembered, both
    /// ways round, and a remembered answer is never asked again: the second
    /// run returns before it reaches the queue holding the opposite answer.
    #[test]
    fn the_prompt_arm_acts_on_the_answer_and_never_asks_a_second_time() {
        use cfgd_core::output::PromptAnswer;
        let keys = vec!["spec.migrationPolicy".to_string()];

        for accepted in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let state = tempfile::tempdir().unwrap();
            let path = dir.path().join("cfgd.yaml");
            std::fs::write(&path, BEHIND_DOC).unwrap();
            let cli = cli_with_config(&path, Some(state.path()));

            let (printer, _buf) = prompting_printer(vec![PromptAnswer::Confirm(accepted)]);
            gate(&printer, &cli, Some(MigrationPolicy::Prompt), false);
            printer.flush();

            let after = std::fs::read_to_string(&path).unwrap();
            assert_eq!(
                after != BEHIND_DOC,
                accepted,
                "a yes writes the alignment and a no leaves the bytes alone: {after}"
            );
            let store = StateStore::open_in_dir(state.path()).unwrap();
            assert_eq!(
                store
                    .migration_answer(&path, cfgd_core::API_VERSION, &keys)
                    .unwrap(),
                Some(accepted),
                "the answer given is the answer held"
            );
            drop(store);

            // The second run holds the OPPOSITE answer. Reaching the prompt at
            // all would pop it; returning on the recorded one leaves it in the
            // queue and the document exactly as the first run left it.
            let (again, _buf2) = prompting_printer(vec![PromptAnswer::Confirm(!accepted)]);
            gate(&again, &cli, Some(MigrationPolicy::Prompt), false);
            again.flush();
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                after,
                "a question already answered is not asked again"
            );
            assert_eq!(
                again.prompt_confirm("still queued?").ok(),
                Some(!accepted),
                "the queued answer was never reached"
            );
        }
    }

    /// A state root nothing has written to holds no answer, so the gate does
    /// not create one to look: a read-only verb on a machine that has never
    /// recorded anything leaves the disk as it found it. A root that DOES
    /// hold an answer comes back with the store the caller can reuse.
    #[test]
    fn a_state_root_nothing_wrote_to_is_not_created_to_read_an_answer_from() {
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfgd.yaml");
        std::fs::write(&path, BEHIND_DOC).unwrap();
        let cli = cli_with_config(&path, Some(state.path()));
        let keys = vec!["spec.migrationPolicy".to_string()];
        let printer = cfgd_core::test_helpers::test_printer();
        let db = state.path().join(cfgd_core::state::STATE_DB_FILENAME);

        let (held, recorded) = consult_recorded(
            &printer,
            &GateInvocation::of(&cli, false),
            &path,
            cfgd_core::API_VERSION,
            &keys,
        )
        .expect("a resolvable state root answers");
        assert!(held.is_none(), "nothing was opened");
        assert_eq!(recorded, None, "and nothing was read");
        assert!(!db.exists(), "no state root was created to read an answer");

        let store = StateStore::open_in_dir(state.path()).unwrap();
        store
            .record_migration_answer(&path, cfgd_core::API_VERSION, false, &keys)
            .unwrap();
        drop(store);

        let (held, recorded) = consult_recorded(
            &printer,
            &GateInvocation::of(&cli, false),
            &path,
            cfgd_core::API_VERSION,
            &keys,
        )
        .expect("a resolvable state root answers");
        assert!(held.is_some(), "a store that holds an answer comes back");
        assert_eq!(recorded, Some(false), "and the answer with it");
    }

    /// The two failures `consult_recorded` can report are worded apart: a
    /// state directory that could not be RESOLVED never reached a store, so
    /// calling it an open failure sends the reader to a file that was never
    /// named. Resolution is what fails when nothing overrides the state
    /// directory and no home can be found for the default.
    #[test]
    #[serial_test::serial]
    fn a_state_directory_that_cannot_be_resolved_is_not_reported_as_a_store_that_would_not_open() {
        use cfgd_core::output::{Printer, Verbosity};
        use cfgd_core::test_helpers::EnvVarGuard;

        let _state = EnvVarGuard::unset("CFGD_STATE_DIR");
        let _systemd = EnvVarGuard::unset("STATE_DIRECTORY");
        let _home = EnvVarGuard::unset("HOME");
        let _profile = EnvVarGuard::unset("USERPROFILE");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfgd.yaml");
        std::fs::write(&path, BEHIND_DOC).unwrap();
        let cli = cli_with_config(&path, None);
        let (printer, _stdout, stderr) = Printer::for_test_split_streams(Verbosity::Normal);

        let answered = consult_recorded(
            &printer,
            &GateInvocation::of(&cli, false),
            &path,
            cfgd_core::API_VERSION,
            &["spec.migrationPolicy".to_string()],
        );
        printer.flush();

        assert!(
            answered.is_none(),
            "a state directory that cannot be resolved answers nothing"
        );
        let text = cfgd_core::test_helpers::captured_text(&stderr);
        assert!(
            text.contains("Could not resolve the state directory"),
            "the resolution failure names resolution: {text}"
        );
        assert!(
            !text.contains("Could not open the state store"),
            "nothing was opened, so nothing reports an open failure: {text}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            BEHIND_DOC,
            "a gate that could not read an answer wrote nothing"
        );
    }

    /// A state store that cannot answer the migration question says so, on
    /// the path a reader actually reaches it by. The reader is asked either
    /// way: a silent read failure means being asked every run with nothing on
    /// screen to fix, and a silent write failure means the answer just given
    /// is thrown away.
    #[test]
    fn a_store_that_cannot_hold_the_answer_reports_instead_of_swallowing() {
        use cfgd_core::output::PromptAnswer;
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfgd.yaml");
        std::fs::write(&path, BEHIND_DOC).unwrap();
        let cli = cli_with_config(&path, Some(state.path()));
        let keys = vec!["spec.migrationPolicy".to_string()];

        // The row is recorded first so the store exists for the gate to open,
        // and the table is then taken away under it: the migrations are gated
        // on the schema version, so reopening does not put it back.
        let store = StateStore::open_in_dir(state.path()).unwrap();
        store
            .record_migration_answer(&path, cfgd_core::API_VERSION, false, &keys)
            .unwrap();
        store.drop_config_migrations_table().unwrap();
        drop(store);

        let (printer, buf) = prompting_printer(vec![PromptAnswer::Confirm(false)]);
        gate(&printer, &cli, Some(MigrationPolicy::Prompt), false);
        printer.flush();

        let text = cfgd_core::test_helpers::captured_text(&buf);
        assert!(
            text.contains("Could not read the recorded migration answer")
                && text.contains("Could not record the migration answer"),
            "both the read and the write say what the store said: {text}"
        );
        assert_eq!(
            text.matches("no such table").count(),
            2,
            "each half carries what the store said, not a wording of its own: {text}"
        );
    }

    /// With nothing overriding it, the gate answers to the policy the
    /// document itself declares — read off the one parse it already makes,
    /// with no second load. An override outranks that field.
    #[test]
    #[serial_test::serial]
    fn the_gate_reads_the_stored_policy_off_its_own_parse_and_an_override_outranks_it() {
        use cfgd_core::output::{Printer, Verbosity};
        let _unset = cfgd_core::test_helpers::EnvVarGuard::unset("CFGD_MIGRATION_POLICY");
        let stored = |declared: &str, over: Option<MigrationPolicy>| {
            let dir = tempfile::tempdir().unwrap();
            let state = tempfile::tempdir().unwrap();
            let path = dir.path().join("cfgd.yaml");
            std::fs::write(
                &path,
                format!(
                    "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: t\nspec:\n  {declared}\n"
                ),
            )
            .unwrap();
            let cli = cli_with_config(&path, Some(state.path()));
            let (printer, _stdout, stderr) = Printer::for_test_split_streams(Verbosity::Normal);
            gate(&printer, &cli, over, false);
            printer.flush();
            let after = std::fs::read_to_string(&path).unwrap();
            (
                after.contains("fileStrategy"),
                cfgd_core::test_helpers::captured_text(&stderr),
            )
        };

        let (wrote, said) = stored("migrationPolicy: Ignore", None);
        assert!(!wrote, "a stored Ignore writes nothing");
        assert!(said.trim().is_empty(), "and says nothing: {said}");

        let (wrote, said) = stored("migrationPolicy: Warn", None);
        assert!(!wrote, "a stored Warn writes nothing");
        assert!(
            said.contains("cfgd config migrate"),
            "a stored Warn reports: {said}"
        );

        let (wrote, _said) = stored("migrationPolicy: Ignore", Some(MigrationPolicy::Update));
        assert!(wrote, "the invocation's own policy outranks the document's");
    }

    /// A daemon never blocks on a prompt and never rewrites a file something
    /// else tracks, so both writing arms fold to a report; off the daemon
    /// nothing folds. Every pair is written out.
    #[test]
    fn a_daemon_folds_both_writing_policies_to_a_report_and_nothing_else() {
        let expected = [
            (false, MigrationPolicy::Prompt, MigrationPolicy::Prompt),
            (false, MigrationPolicy::Warn, MigrationPolicy::Warn),
            (false, MigrationPolicy::Update, MigrationPolicy::Update),
            (false, MigrationPolicy::Ignore, MigrationPolicy::Ignore),
            (true, MigrationPolicy::Prompt, MigrationPolicy::Warn),
            (true, MigrationPolicy::Warn, MigrationPolicy::Warn),
            (true, MigrationPolicy::Update, MigrationPolicy::Warn),
            (true, MigrationPolicy::Ignore, MigrationPolicy::Ignore),
        ];
        for (is_daemon, policy, want) in expected {
            assert_eq!(
                daemon_folded_policy(is_daemon, policy),
                want,
                "is_daemon={is_daemon}, policy={policy:?}"
            );
        }
    }

    /// Every `config` subcommand clap offers says whether the load-time gate
    /// runs for it, against a hand-written expectation: a verb added later
    /// fails this walk until it is classified. `set` and `unset` appear twice,
    /// because which key they name is what decides them.
    #[test]
    fn every_config_subcommand_says_whether_the_migration_gate_runs_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfgd.yaml");
        // (the clap name, the argv, whether the gate is withheld)
        let cases: &[(&str, &[&str], bool)] = &[
            ("show", &["config", "show"], false),
            ("get", &["config", "get", "theme.name"], false),
            ("set", &["config", "set", "theme.name", "dracula"], false),
            ("unset", &["config", "unset", "theme.name"], false),
            ("edit", &["config", "edit"], true),
            ("migrate", &["config", "migrate"], true),
            ("set", &["config", "set", "migrationPolicy", "Ignore"], true),
            (
                "set",
                &["config", "set", "spec.migrationPolicy", "Ignore"],
                true,
            ),
            ("unset", &["config", "unset", "migrationPolicy"], true),
        ];

        use clap::CommandFactory;
        let command = Cli::command();
        let config = command
            .get_subcommands()
            .find(|sub| sub.get_name() == "config")
            .expect("cfgd config is a subcommand");
        let names: Vec<&str> = config.get_subcommands().map(|sub| sub.get_name()).collect();
        assert!(
            names.len() >= 6,
            "the walk found only {} config subcommands: {names:?}",
            names.len()
        );
        for name in &names {
            assert!(
                cases.iter().any(|(case, _, _)| case == name),
                "`cfgd config {name}` is not classified: say whether the migration gate runs for it"
            );
        }

        for (name, argv, withheld) in cases {
            assert!(
                names.contains(name),
                "the case names `cfgd config {name}`, which clap does not offer"
            );
            let cli = cli_running(&path, None, argv);
            assert_eq!(
                gate_exempt(cli.command.as_ref()).is_some(),
                *withheld,
                "`cfgd {}`",
                argv.join(" ")
            );
        }

        // A verb that is not `config` at all is never withheld.
        let cli = cli_running(&path, None, &["paths"]);
        assert!(
            gate_exempt(cli.command.as_ref()).is_none(),
            "the gate runs for every reader the advisory is written for"
        );
        for (name, argv, withheld) in cases {
            if !withheld {
                continue;
            }
            let cli = cli_running(&path, None, argv);
            assert!(
                gate_exempt(cli.command.as_ref()).is_some_and(|why| !why.is_empty()),
                "`cfgd config {name}` is withheld without saying why"
            );
        }
    }

    /// Every write parses the document the closure leaves before it aligns or
    /// replaces anything, so a value the parser refuses leaves the file as it
    /// was. `Patch` is the one global `fileStrategy` the parser rejects, and it
    /// lands on a key the alignment would otherwise have materialized.
    #[test]
    fn a_value_the_parser_refuses_leaves_the_document_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfgd.yaml");
        std::fs::write(&path, BEHIND_DOC).unwrap();
        let cfg = cfgd_core::config::parse_config(BEHIND_DOC, &path).unwrap();
        assert!(
            pending_alignment(&cfg, BEHIND_DOC, Path::new("cfgd.yaml"))
                .keys
                .contains(&"spec.fileStrategy".to_string()),
            "the key the refusal lands on is one the alignment would write"
        );

        let err = mutate_config_yaml(&path, |raw| {
            let (spec, leaf) =
                crate::cli::config_cmd::walk_yaml_path_mut(raw, "spec.fileStrategy")?;
            spec.insert(leaf.into(), "Patch".into());
            Ok(())
        })
        .expect_err("the parser refuses Patch");
        assert!(
            format!("{err}").contains("fileStrategy"),
            "the refusal names the field: {err}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            BEHIND_DOC,
            "a refused write replaces nothing"
        );
    }

    /// A document behind the schema by one key of a section it already holds
    /// stays behind by exactly that key through every writer, whatever the
    /// gate did about it: `Warn` and `Ignore` leave it, and a recorded "no" is
    /// the reader declining the alignment. A write fills only what it left
    /// undeclared itself, so none of the three writers answers the question
    /// the reader was asked, and none adds anything beyond its own subject.
    #[test]
    fn a_write_leaves_a_key_the_reader_left_undeclared_undeclared() {
        #[derive(Clone, Copy, Debug)]
        enum Handling {
            Warn,
            Ignore,
            RecordedNo,
        }
        #[derive(Clone, Copy, Debug)]
        enum Writer {
            ConfigSet,
            SourceAdd,
            ProfileSwitch,
        }
        const BEHIND_BY: &str = "spec.fileStrategy";

        let mut offenders = Vec::new();
        for handling in [Handling::Warn, Handling::Ignore, Handling::RecordedNo] {
            for writer in [Writer::ConfigSet, Writer::SourceAdd, Writer::ProfileSwitch] {
                let case = format!("{handling:?} then {writer:?}");
                let dir = tempfile::tempdir().unwrap();
                let state = tempfile::tempdir().unwrap();
                let printer = cfgd_core::test_helpers::test_printer();
                crate::cli::init::cmd_init::scaffold(dir.path(), Some("t"), None, &printer)
                    .unwrap();
                let path = dir.path().join("cfgd.yaml");
                let mut tree: serde_yaml::Value =
                    serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
                let spec = tree["spec"].as_mapping_mut().unwrap();
                spec.remove("fileStrategy");
                spec.insert("profile".into(), "default".into());
                std::fs::write(&path, serde_yaml::to_string(&tree).unwrap()).unwrap();
                std::fs::write(
                    dir.path().join("profiles").join("other.yaml"),
                    "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: other\nspec: {}\n",
                )
                .unwrap();
                let pending_now = || {
                    let bytes = std::fs::read_to_string(&path).unwrap();
                    let cfg = cfgd_core::config::parse_config(&bytes, &path).unwrap();
                    pending_alignment(&cfg, &bytes, &path).keys
                };
                assert_eq!(
                    pending_now(),
                    vec![BEHIND_BY.to_string()],
                    "{case}: the fixture is behind by one key of the spec section"
                );

                let cli = cli_running(&path, Some(state.path()), &["status"]);
                let policy = match handling {
                    Handling::Warn => MigrationPolicy::Warn,
                    Handling::Ignore => MigrationPolicy::Ignore,
                    Handling::RecordedNo => {
                        StateStore::open_in_dir(state.path())
                            .unwrap()
                            .record_migration_answer(
                                &path,
                                cfgd_core::API_VERSION,
                                false,
                                &[BEHIND_BY.to_string()],
                            )
                            .unwrap();
                        MigrationPolicy::Prompt
                    }
                };
                let before_gate = std::fs::read_to_string(&path).unwrap();
                gate(&printer, &cli, Some(policy), false);
                assert_eq!(
                    std::fs::read_to_string(&path).unwrap(),
                    before_gate,
                    "{case}: the gate leaves the document as the reader left it"
                );

                let before: serde_yaml::Value = serde_yaml::from_str(&before_gate).unwrap();
                let subject = match writer {
                    Writer::ConfigSet => {
                        crate::cli::config_cmd::cmd_config_set(&cli, &printer, "profile", "other")
                            .unwrap();
                        "profile"
                    }
                    Writer::SourceAdd => {
                        let source = cfgd_core::config::SourceSpec {
                            name: "acme".to_string(),
                            origin: serde_yaml::from_str(
                                "type: Git\nurl: https://example.com/x.git\n",
                            )
                            .unwrap(),
                            subscription: Default::default(),
                            sync: Default::default(),
                        };
                        crate::cli::add_source_to_config(&path, &source).unwrap();
                        "sources"
                    }
                    Writer::ProfileSwitch => {
                        crate::cli::profile::cmd_profile_switch(&cli, "other", &printer).unwrap();
                        "profile"
                    }
                };

                let pending = pending_now();
                if pending != [BEHIND_BY] {
                    offenders.push(format!("{case}: pending after the write {pending:?}"));
                }
                let after: serde_yaml::Value =
                    serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
                let without_subject = |doc: &serde_yaml::Value| {
                    let mut doc = doc.clone();
                    doc["spec"].as_mapping_mut().unwrap().remove(subject);
                    doc
                };
                if without_subject(&after) != without_subject(&before) {
                    offenders.push(format!(
                        "{case}: the write changed more than spec.{subject}:\n{}",
                        serde_yaml::to_string(&after).unwrap()
                    ));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "a write answered the question the reader was asked:\n{}",
            offenders.join("\n")
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
        let pending = pending_alignment(&cfg, doc, Path::new("cfgd.yaml"));
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
            !pending_alignment(&cfg, &aligned, Path::new("cfgd.yaml"))
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
        let pending = pending_alignment(&cfg, doc, Path::new("cfgd.yaml"));
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
        let pending = pending_alignment(&cfg, doc, Path::new("cfgd.yaml"));
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
        let pending = pending_alignment(&cfg, doc, Path::new("cfgd.yaml"));
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
        let pending = pending_alignment(&cfg, &written, Path::new("cfgd.yaml"));
        assert!(
            pending.keys.is_empty(),
            "a document carrying every reported key is behind by nothing: {:?}",
            pending.keys
        );
    }

    /// A document declaring the LEGACY flat `spec.theme` (no `spec.output`
    /// block at all) already declares the fact `spec.output.theme` would
    /// restate. `pending_alignment` must not offer to materialize it: the
    /// prior defect wrote `spec.output.theme` onto exactly this document
    /// without removing `spec.theme`, which is what made the next load
    /// report `spec.theme and spec.output.theme are both set` for a document
    /// that only ever declared one of them.
    #[test]
    fn a_legacy_flat_key_is_not_reported_as_a_pending_nested_one() {
        // One fixture value per LEGACY_OUTPUT_KEYS entry, keyed by the flat
        // spelling, so a key added to the table trips this walk until it
        // gains a fixture rather than passing by omission.
        let legacy_values: &[(&str, &str)] =
            &[("spec.theme", "dracula"), ("spec.usageHints", "true")];
        for (old, _) in cfgd_core::config::LEGACY_OUTPUT_KEYS {
            let value = legacy_values
                .iter()
                .find(|(k, _)| k == old)
                .map(|(_, v)| *v)
                .unwrap_or_else(|| panic!("no fixture value for legacy key {old}"));
            let flat_key = old.strip_prefix("spec.").unwrap_or(old);
            let doc = format!(
                "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  {flat_key}: {value}\n"
            );
            let cfg =
                cfgd_core::config::parse_config(&doc, std::path::Path::new("cfgd.yaml")).unwrap();
            let pending = pending_alignment(&cfg, &doc, Path::new("cfgd.yaml"));
            let (_, new) = cfgd_core::config::LEGACY_OUTPUT_KEYS
                .iter()
                .find(|(o, _)| o == old)
                .unwrap();
            assert!(
                !pending
                    .keys
                    .iter()
                    .any(|k| k == new || k.starts_with(&format!("{new}."))),
                "a legacy {old} document already declares the fact {new} would restate; \
                 the writer must not add it beside {old}: {:?}",
                pending.keys
            );
        }
    }
}
