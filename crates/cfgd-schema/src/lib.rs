//! Config value types whose shape the local YAML parser and the Module CRD
//! schema must agree on.
//!
//! Both halves of cfgd describe the same file: `cfgd-core`'s parser reads it
//! out of `module.yaml`, and the cluster-side Module CRD accepts it as part of
//! a Kubernetes resource. Owning the types here — with serde and schemars as
//! the only dependencies — is what lets the CRD reuse them without the CSI node
//! plugin, which builds `cfgd-core` with `default-features = false`, inheriting
//! the kube/k8s-openapi stack through them.

mod enum_de;

// `case_insensitive_enum!` is `#[macro_export]`ed, so its expansion lands in
// crates that need not depend on serde themselves; `$crate::serde` is how it
// names the one this crate already has.
#[doc(hidden)]
pub use serde;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// File deployment strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, schemars::JsonSchema)]
pub enum FileStrategy {
    /// Create a symbolic link from target to source (default).
    #[default]
    Symlink,
    /// Copy source content to target.
    Copy,
    /// Render a Tera template and write the output (auto-selected for .tera files).
    Template,
    /// Create a hard link from target to source.
    Hardlink,
    /// Merge structured keys/values into the target, or pipe it through a
    /// script, leaving everything else untouched. Requires a `patch:` block.
    Patch,
}

case_insensitive_enum!(FileStrategy {
    "Symlink" => FileStrategy::Symlink,
    "Copy" => FileStrategy::Copy,
    "Template" => FileStrategy::Template,
    "Hardlink" => FileStrategy::Hardlink,
    "Patch" => FileStrategy::Patch,
});

impl FileStrategy {
    /// Whether the strategy is meaningful as the global `spec.fileStrategy`
    /// default.
    ///
    /// `Patch` is not: it is defined by a per-file `patch:` block, which a
    /// file inheriting the global default cannot have. The config parser and
    /// the published schema both derive their accepted value set from this, so
    /// an editor and `cfgd` can never disagree about it.
    pub fn valid_as_global_default(self) -> bool {
        !matches!(self, FileStrategy::Patch)
    }

    /// The lowercase word a report names this strategy by — a deploy row's
    /// child method, and the status table's Method column. Distinct from
    /// [`Self::as_str`], the canonical PascalCase wire/schema spelling: this
    /// is the ONE display spelling, so the two surfaces naming a resolved
    /// strategy cannot drift on casing the way an inline
    /// `.as_str().to_lowercase()` at each call site would invite.
    pub fn method_label(self) -> &'static str {
        match self {
            FileStrategy::Symlink => "symlink",
            FileStrategy::Copy => "copy",
            FileStrategy::Template => "template",
            FileStrategy::Hardlink => "hardlink",
            FileStrategy::Patch => "patch",
        }
    }

    /// The strategy a `module_file_manifest.strategy` column records, read
    /// back — the inverse of [`Self::as_str`], which is what the manifest
    /// writer persists. `None` for a value no variant spells, so a corrupt
    /// column degrades to an absent cell rather than a guessed method.
    pub fn from_recorded(recorded: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|s| recorded.eq_ignore_ascii_case(s.as_str()))
    }
}

/// File format used to interpret and re-serialize a `Patch`-strategy target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub enum PatchFormat {
    /// INI sections/keys, edited line-by-line to preserve comments and layout.
    Ini,
    /// JSON, re-serialized on write (no comments to preserve).
    Json,
    /// YAML; comments are NOT preserved across a merge (see docs for the caveat).
    Yaml,
    /// TOML, edited in place to preserve comments and layout.
    Toml,
}

case_insensitive_enum!(PatchFormat {
    "Ini" => PatchFormat::Ini,
    "Json" => PatchFormat::Json,
    "Yaml" => PatchFormat::Yaml,
    "Toml" => PatchFormat::Toml,
});

/// Configuration for the `Patch` file strategy: a structured merge (`ensure`)
/// or a content-rewriting script, applied on top of the target's current
/// content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PatchSpec {
    /// File format to parse the target as. Inferred from the target's
    /// extension when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<PatchFormat>,
    /// Keys/values to deep-merge into the target, leaving unmentioned keys
    /// untouched. Values are literal (no template rendering). Mutually
    /// exclusive with `script`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<serde_json::Value>")]
    pub ensure: Option<serde_yaml::Value>,
    /// A script path or an inline command that receives the target's current
    /// content on stdin and writes the new content to stdout. A relative path
    /// resolves against the module directory for a module file
    /// (`spec.files[]`) and against the config directory for a profile file
    /// (`spec.files.managed[]`); a value that resolves to no file is run as an
    /// inline command. Mutually exclusive with `ensure`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
    /// Name of the source whose `constraints.noScripts` bars this filter, set
    /// by composition when the subscriber did not opt in.
    ///
    /// Not part of the config surface (`#[serde(skip)]`, so `deny_unknown_fields`
    /// rejects it in YAML and it never reaches the published schema): composition
    /// is the only writer. Poisoning the spec rather than dropping it keeps the
    /// file visible on read-only surfaces while making the filter unrunnable by
    /// construction — every evaluation path funnels through `compute_patched`,
    /// which refuses a marked spec.
    #[serde(skip)]
    pub blocked_by: Option<String>,
}

/// Controls when encryption is required for a managed file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, schemars::JsonSchema)]
pub enum EncryptionMode {
    /// File must be encrypted when stored in the repository.
    #[default]
    InRepo,
    /// File must always be encrypted, including at rest on disk.
    Always,
}

case_insensitive_enum!(EncryptionMode {
    "InRepo" => EncryptionMode::InRepo,
    "Always" => EncryptionMode::Always,
});

/// Encryption settings for a managed file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EncryptionSpec {
    /// The encryption backend to use (e.g. "sops", "age").
    pub backend: String,
    /// When encryption must be enforced. Defaults to `InRepo`.
    #[serde(default)]
    pub mode: EncryptionMode,
}

/// Interpreter for inline lifecycle scripts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ScriptShell {
    /// Platform default: `sh` on Unix, `cmd.exe` on Windows.
    #[default]
    Auto,
    Sh,
    Bash,
    Zsh,
    Pwsh,
    Cmd,
}

case_insensitive_enum!(ScriptShell {
    "auto" => ScriptShell::Auto,
    "sh" => ScriptShell::Sh,
    "bash" => ScriptShell::Bash,
    "zsh" => ScriptShell::Zsh,
    "pwsh" => ScriptShell::Pwsh,
    "cmd" => ScriptShell::Cmd,
});

/// A lifecycle script entry: either a bare command string, or a mapping for
/// one that needs a timeout, shell, or guard condition.
///
/// ```yaml
/// preApply: "echo starting"
/// # or
/// postApply:
///   run: brew update
///   timeout: 2m
///   onlyIf: command -v brew
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum ScriptEntry {
    /// A bare command string, run with the platform's default shell and no
    /// timeout/guard.
    Simple(String),
    /// The mapping form, carrying the body and its knobs.
    // A named type rather than an inline variant so `cfgd explain` shows a
    // reader `<(string | ScriptCommand)>` — a name they can look up — instead
    // of `<(string | object)>`.
    Full(ScriptCommand),
}

/// The mapping form of a script entry: a command with a timeout, shell,
/// guard condition or working directory.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ScriptCommand {
    /// The command or script body to run.
    pub run: String,
    /// Kill the script if it runs longer than this duration (`"30s"`, `"2m"`).
    /// Unset means no timeout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
    /// Kill the script if it produces no stdout/stderr output for this duration.
    /// Prevents scripts from silently hanging on unresponsive resources.
    /// Format: "30s", "2m", etc. If unset, no idle timeout is enforced.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "idleTimeout"
    )]
    pub idle_timeout: Option<String>,
    /// Treat a non-zero exit as success and continue reconciliation instead
    /// of failing the run. Default: `false`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "continueOnError"
    )]
    pub continue_on_error: Option<bool>,
    /// Interpreter to use for inline commands. Ignored (and rejected) on file scripts.
    #[serde(default, skip_serializing_if = "is_shell_auto")]
    pub shell: ScriptShell,
    /// Run the script only if this command exits zero. A non-zero exit skips
    /// the script (the condition for running was not met). Evaluated with the
    /// same shell, working directory, and environment as the body.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "onlyIf")]
    pub only_if: Option<String>,
    /// Run the script only if this command exits NON-zero. A zero exit
    /// (success) skips the script (the guarded state already holds).
    /// Evaluated with the same shell, working directory, and environment as
    /// the body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unless: Option<String>,
    /// Skip the script if this path already exists. A leading `~` expands to
    /// the home directory; a relative path resolves against the script's
    /// working directory. Existence follows symlinks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creates: Option<String>,
    /// Run the script attached to the terminal (inherited stdin/stdout/stderr,
    /// no spinner, no output capture, no idle timeout) so it can prompt the
    /// user — e.g. `echo "press Enter when done"; read`. Requires a TTY: when
    /// stdin is not a terminal (CI, piped input, or any daemon-run phase) the
    /// script is skipped with a warning rather than hanging on instant EOF.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub interactive: bool,
    /// Working directory for the script. By default every lifecycle script
    /// runs in the user's home directory — never the config source tree — so
    /// a relative write can't pollute the user's GitOps repo. Set `workdir`
    /// to override: a leading `~` expands to home and `$VAR`/`${VAR}` expand
    /// against the script environment (which always carries `$CFGD_MODULE_DIR`
    /// and `$CFGD_CONFIG_DIR`), so `workdir: ~/.local/share/app`,
    /// `workdir: $CFGD_MODULE_DIR`, or an absolute path all work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
}

fn is_shell_auto(s: &ScriptShell) -> bool {
    *s == ScriptShell::Auto
}

impl ScriptEntry {
    /// Extract the run command string from any variant.
    pub fn run_str(&self) -> &str {
        match self {
            ScriptEntry::Simple(s) => s,
            ScriptEntry::Full(ScriptCommand { run, .. }) => run,
        }
    }
}

impl std::fmt::Display for ScriptEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.run_str())
    }
}

/// `spec.scripts`: lifecycle hooks run at specific points in the reconcile cycle.
///
/// ```yaml
/// scripts:
///   preApply: "echo starting apply"
///   postApply:
///     - run: brew cleanup
///       continueOnError: true
///   onDrift: "notify-send 'cfgd: drift detected'"
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScriptSpec {
    /// Run once before any action in an apply.
    #[serde(default)]
    pub pre_apply: Vec<ScriptEntry>,
    /// Run once after every action in an apply completes.
    #[serde(default)]
    pub post_apply: Vec<ScriptEntry>,
    /// Run once before a daemon reconcile tick begins.
    #[serde(default)]
    pub pre_reconcile: Vec<ScriptEntry>,
    /// Run once after a daemon reconcile tick completes.
    #[serde(default)]
    pub post_reconcile: Vec<ScriptEntry>,
    /// Run when the daemon detects drift, before any auto-apply decision.
    #[serde(default)]
    pub on_drift: Vec<ScriptEntry>,
    /// Run when a watched file changes on disk (requires `daemon.reconcile.onChange`).
    #[serde(default)]
    pub on_change: Vec<ScriptEntry>,
}

impl ScriptSpec {
    /// Every lifecycle hook paired with the entries declared for it, in the
    /// canonical hook order: each context's `pre` before its `post` (apply,
    /// then reconcile), then the event hooks. An apply and a reconcile are
    /// separate runs, so no single run reaches all six.
    ///
    /// The ONE enumeration of the hook set: a surface that lists, counts or
    /// names hooks reads from here, so none of them can miss a hook the YAML
    /// accepts or disagree about the order they are reported in.
    pub fn hooks(&self) -> [(&'static str, &[ScriptEntry]); 6] {
        // Destructured, so a seventh hook field does not compile until it is
        // listed here — the mechanism behind "no surface can miss a hook".
        let Self {
            pre_apply,
            post_apply,
            pre_reconcile,
            post_reconcile,
            on_drift,
            on_change,
        } = self;
        [
            ("preApply", pre_apply),
            ("postApply", post_apply),
            ("preReconcile", pre_reconcile),
            ("postReconcile", post_reconcile),
            ("onDrift", on_drift),
            ("onChange", on_change),
        ]
    }
}

/// A declarative backup: snapshot `source` (a file or directory) into
/// `destination`, retaining the newest `retention` snapshots.
///
/// The shape is validated at parse time and run by the backup engine.
/// Schedule-less backups (no `schedule`) run automatically on every
/// `cfgd apply`; every backup — scheduled or not — can also be run directly
/// with `cfgd backup run [name]`.
//
// Every `///` line on this struct and its fields is copied verbatim into
// schemas/cfgd-profile.schema.json, which editors render as YAML completion
// help. Keep them plain prose: a rustdoc intra-doc link renders as literal
// `[`name`]` noise to a user who has no rustdoc to follow it to.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackupSpec {
    /// Unique identifier for this backup within `spec.backups`, unique across
    /// the list. Keys the `destination` default, run records, and CLI
    /// selection. Becomes a directory component (`<state_dir>/backups/<name>/`)
    /// and a lock filename (`<state_dir>/locks/backup-<name>.lock`), so it must
    /// be non-empty, non-blank, a single segment (no `/` or `\`), not a
    /// directory reference (`.`, `..`), not rooted (`/daily`, `C:/daily`), and
    /// free of `:` anywhere — a drive and NTFS data-stream separator on Windows.
    /// Windows shapes are rejected on every platform so a name written on one
    /// OS stays valid on the others.
    pub name: String,
    /// File or directory to snapshot. A leading `~` expands to the home
    /// directory. Must not contain, or sit inside, the resolved `destination` —
    /// a nested pair is rejected before any copy, with symlinks resolved on both
    /// sides. Its filename is what `{filename}` interpolates, so a source whose
    /// filename contains `:` (legal on Unix, a drive and data-stream separator
    /// on Windows) needs an explicit `namePattern` that leaves `{filename}` out.
    pub source: PathBuf,
    /// Where snapshots are written. Defaults to `<state_dir>/backups/<name>/`
    /// when omitted — resolved by the backup engine, not at parse time, since
    /// the state dir depends on runtime scope/overrides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<PathBuf>,
    /// Filename template for each snapshot. Supports `{name}`, `{filename}`,
    /// and `{timestamp}` (UTC, `%Y%m%dT%H%M%SZ`). Unknown `{var}` tokens are
    /// rejected at parse time. A literal `/` nests the snapshot in a
    /// subdirectory of the destination. At run time the rendered value must be
    /// relative and every segment must name something: `.` and `..` segments,
    /// empty segments (`a//b`, `daily/`), rooted values (`/daily`, `C:/daily`,
    /// `C:daily`, `\\server\share`), and `:` anywhere are all rejected. Windows
    /// shapes are rejected on every platform, so a pattern is valid everywhere
    /// or nowhere. A rejection names the `{filename}` it interpolated, so a
    /// colon in the source filename points at itself. Defaults to
    /// `"{filename}.{timestamp}"`.
    #[serde(default = "default_backup_name_pattern")]
    pub name_pattern: String,
    /// When to run this backup: a duration interval (e.g. `"6h"`) or a cron
    /// expression, validated at parse time. Cron expressions may be 5-field
    /// (`minute hour day month weekday`, e.g. `"0 3 * * *"`) or 6-field with a
    /// leading seconds field (`second minute hour day month weekday`, e.g.
    /// `"30 0 3 * * *"`), and are evaluated in the machine's LOCAL timezone,
    /// like a crontab entry. An interval is measured from the unit's last
    /// recorded run, so a `"1d"` backup on a machine rebooted daily still fires
    /// daily. Setting this hands the backup to the daemon's timers and takes it
    /// out of apply; omitted means "run on every apply".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    /// Number of newest snapshots to keep for this backup; older snapshots are
    /// pruned from disk and from the run history. Must be at least 1 (`0` would
    /// keep no backups, which is a misconfiguration rather than a supported
    /// "unlimited" mode). Defaults to 10.
    #[serde(default = "default_backup_retention")]
    #[schemars(range(min = 1))]
    pub retention: u32,
    /// Scripts run before the snapshot is taken (e.g. stop a service that
    /// holds `source` open so the snapshot is consistent). A failure skips the
    /// snapshot and records a failed run; `postBackup` still runs.
    #[serde(default)]
    pub pre_backup: Vec<ScriptEntry>,
    /// Scripts run after the copy step (e.g. restart the service stopped by
    /// `preBackup`). Always attempted, including after a failed `preBackup` or
    /// a failed copy.
    #[serde(default)]
    pub post_backup: Vec<ScriptEntry>,
}

/// The `namePattern` a backup takes when it declares none.
pub fn default_backup_name_pattern() -> String {
    "{filename}.{timestamp}".to_string()
}

/// The number of snapshots a backup retains when it declares no `retention`.
pub fn default_backup_retention() -> u32 {
    10
}

/// Why a file entry's `source` / `strategy` / `patch` / `encryption` shape was
/// refused, as a complete sentence naming the entry it judged.
///
/// The message is the whole error: a caller with its own error type wraps this
/// one's `Display` (or its field) rather than re-wording the refusal, so the
/// local YAML parser and the Module CRD state a rejection identically.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct FileShapeError(pub String);

/// Validate the `source` / `strategy` / `patch` / `encryption` shape shared by
/// `ManagedFileSpec` and `ModuleFileEntry`: `source` is required unless
/// `strategy` is `Patch`; a `patch` block is required when `strategy` is
/// `Patch` and rejected otherwise; within a `patch` block exactly one of
/// `ensure`/`script` must be set; `encryption` is rejected on a `Patch` entry.
pub fn validate_file_patch_shape(
    subject: &str,
    source_is_empty: bool,
    strategy: Option<FileStrategy>,
    patch: Option<&PatchSpec>,
    encryption_declared: bool,
    private: bool,
) -> Result<(), FileShapeError> {
    let is_patch = matches!(strategy, Some(FileStrategy::Patch));
    // `private` marks the SOURCE file local-only (gitignored, skipped where it
    // is absent). `Patch` has no source, so the flag can only ever be a no-op
    // that reads as a promise the strategy never keeps.
    if is_patch && private {
        return Err(FileShapeError(format!(
            "{subject}: 'private' is not supported with strategy 'patch'"
        )));
    }
    // Every `encryption` mode constrains the SOURCE file a strategy deploys
    // ("must be encrypted in the repo"). `Patch` has no source — it rewrites
    // the target's own plaintext structure — so the constraint could only be
    // silently ignored. Reject it instead of pretending it was honoured.
    if is_patch && encryption_declared {
        return Err(FileShapeError(format!(
            "{subject}: 'encryption' is not supported with strategy 'patch'"
        )));
    }
    match (is_patch, patch) {
        (true, None) => Err(FileShapeError(format!(
            "{subject}: strategy 'patch' requires a 'patch' block"
        ))),
        (false, Some(_)) => Err(FileShapeError(format!(
            "{subject}: 'patch' is only valid when strategy is 'patch'"
        ))),
        (true, Some(m)) => match (m.ensure.is_some(), m.script.is_some()) {
            (true, true) => Err(FileShapeError(format!(
                "{subject}: 'patch' must set exactly one of 'ensure' or 'script', not both"
            ))),
            (false, false) => Err(FileShapeError(format!(
                "{subject}: 'patch' must set exactly one of 'ensure' or 'script'"
            ))),
            _ => Ok(()),
        },
        (false, None) => {
            if source_is_empty {
                Err(FileShapeError(format!(
                    "{subject}: 'source' is required unless strategy is 'patch'"
                )))
            } else {
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_patch_entry_declaring_encryption_is_refused() {
        let patch = PatchSpec {
            format: None,
            ensure: None,
            script: Some("cat".to_string()),
            blocked_by: None,
        };
        let err = validate_file_patch_shape(
            "module file 'x'",
            true,
            Some(FileStrategy::Patch),
            Some(&patch),
            true,
            false,
        )
        .expect_err("encryption on a patch entry must be refused");
        assert_eq!(
            err.to_string(),
            "module file 'x': 'encryption' is not supported with strategy 'patch'"
        );
    }

    #[test]
    fn a_patch_format_parses_case_insensitively_and_serializes_canonically() {
        let parsed: PatchFormat = serde_yaml::from_str("yaml").expect("lowercase token parses");
        assert_eq!(parsed, PatchFormat::Yaml);
        let rendered = serde_yaml::to_string(&parsed).expect("serialize");
        assert_eq!(rendered.trim(), "Yaml");
    }

    #[test]
    fn a_backup_spec_round_trips_with_its_script_entries() {
        let yaml = "\
name: db
source: /var/lib/db
preBackup:
- run: x
  onlyIf: y
";
        let spec: BackupSpec = serde_yaml::from_str(yaml).expect("backup spec parses");
        assert_eq!(
            spec.pre_backup,
            vec![ScriptEntry::Full(ScriptCommand {
                run: "x".to_string(),
                only_if: Some("y".to_string()),
                ..ScriptCommand::default()
            })]
        );
        let rendered = serde_yaml::to_string(&spec).expect("serialize");
        assert!(
            rendered.contains("preBackup:\n- run: x\n  onlyIf: y\n"),
            "script entries must survive the round trip: {rendered}"
        );
    }
}
