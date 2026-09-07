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

/// Which layer owns a backup unit's schedule.
///
/// `Cluster` (the default) leaves the unit open to a cluster `BackupPolicy`,
/// which may set or replace its `schedule` and `retention`. `Local` pins the
/// unit to the machine: the policy still reports it, but projects no schedule
/// onto it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, schemars::JsonSchema)]
pub enum ScheduleOwner {
    /// A cluster `BackupPolicy` may override this unit's schedule (default).
    #[default]
    Cluster,
    /// The profile keeps this unit's schedule; no policy projects onto it.
    Local,
}

case_insensitive_enum!(ScheduleOwner {
    "Cluster" => ScheduleOwner::Cluster,
    "Local" => ScheduleOwner::Local,
});

impl ScheduleOwner {
    /// The lowercase word a listing's Schedule Owner cell shows. Distinct from
    /// [`Self::as_str`], the canonical PascalCase wire/schema spelling: this is
    /// the ONE display spelling, so every surface naming the owning layer
    /// cannot drift on casing the way an inline `.as_str().to_lowercase()` at
    /// each call site would invite.
    pub fn label(self) -> &'static str {
        match self {
            ScheduleOwner::Cluster => "cluster",
            ScheduleOwner::Local => "local",
        }
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
    /// Which layer owns this unit's schedule. `Cluster` (the default) lets a
    /// cluster `BackupPolicy` set or replace this unit's `schedule` and
    /// `retention`; `Local` pins the unit to the machine, so a policy reports
    /// it but projects no schedule onto it. Parsed case-insensitively.
    #[serde(default)]
    pub schedule_owner: ScheduleOwner,
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

/// Refuse a file entry whose `target` cannot key a server-side-apply merge:
/// an empty one, or one an earlier entry in the same list already claimed.
///
/// The Module CRD declares `spec.files` an SSA map keyed by `target`, so two
/// entries sharing a target make the API server refuse the whole resource with
/// a message naming neither of them. Both refusals name their entry by the
/// caller's own `subject`. A duplicate is a fact about the PAIR: where the two
/// subjects differ the message names both halves of the collision, and where
/// the caller derives its subject from the target itself, so both halves spell
/// the same thing, it says the entry was declared twice instead. `seen` is the
/// caller's own map, carried across its loop, so one pass answers both
/// questions for a whole list.
pub fn validate_file_target<'a>(
    subject: &str,
    target: &'a str,
    seen: &mut std::collections::HashMap<&'a str, String>,
) -> Result<(), FileShapeError> {
    if target.is_empty() {
        return Err(FileShapeError(format!(
            "{subject}: target must not be empty"
        )));
    }
    if let Some(first) = seen.get(target) {
        return Err(FileShapeError(if first == subject {
            format!("{subject}: declared twice")
        } else {
            format!("{subject}: target '{target}' duplicates {first}")
        }));
    }
    seen.insert(target, subject.to_string());
    Ok(())
}

/// Reject a `platforms:` tag no host can ever match.
///
/// A platform tag is compared verbatim, so a misspelled one silently matches
/// nothing: on a whole module that is at least a visible Skip action, but on
/// one env var it is a variable that quietly never appears. Every tag cfgd
/// emits is lowercase `[a-z0-9_]`, and the four families of near-miss spelling
/// (`darwin`, `win`, `amd64`, `arm64`) are named against their canonical token
/// rather than merely refused.
///
/// Anything else lowercase is accepted: a distro or arch cfgd does not name is
/// still a legitimate tag for another host.
pub fn validate_platform_tag(tag: &str) -> Result<(), String> {
    let canonical = |t: &str| match t {
        "darwin" | "osx" | "mac" => Some("macos"),
        "win" | "win32" | "win64" => Some("windows"),
        "x64" | "amd64" => Some("x86_64"),
        "arm64" => Some("aarch64"),
        _ => None,
    };
    let lower = tag.to_ascii_lowercase();
    if let Some(canon) = canonical(&lower) {
        return Err(format!(
            "platform tag '{tag}' is not a platform: tags are matched exactly; use '{canon}'"
        ));
    }
    if tag.is_empty()
        || !tag
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err(format!(
            "platform tag '{tag}' is not a platform: tags are matched exactly and every tag cfgd \
             knows is lowercase letters, digits and underscores (for example 'macos', 'ubuntu', 'x86_64')"
        ));
    }
    Ok(())
}

/// The serde hook every `platforms:` field is deserialized through, so a tag
/// no host can match is refused where it is written rather than at the machine
/// it silently skipped.
pub fn deserialize_platform_tags<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let tags = Vec::<String>::deserialize(deserializer)?;
    for tag in &tags {
        validate_platform_tag(tag).map_err(serde::de::Error::custom)?;
    }
    Ok(tags)
}

// ---------------------------------------------------------------------------
// Backup unit grammar
// ---------------------------------------------------------------------------

/// Parse a duration string like "30s", "5m", "1h", or a plain number (as seconds).
///
/// Returns an error description on invalid input.
pub fn parse_duration_str(s: &str) -> Result<std::time::Duration, String> {
    let s = s.trim();
    const SUFFIXES: &[(char, u64)] = &[('s', 1), ('m', 60), ('h', 3600), ('d', 86400)];
    for &(suffix, multiplier) in SUFFIXES {
        if let Some(n) = s.strip_suffix(suffix) {
            return n
                .trim()
                .parse::<u64>()
                .map(|v| std::time::Duration::from_secs(v * multiplier))
                .map_err(|_| format!("invalid timeout: {}", s));
        }
    }
    s.parse::<u64>()
        .map(std::time::Duration::from_secs)
        .map_err(|_| format!("invalid timeout '{}': use 30s, 5m, or 1h", s))
}

/// Validate that `raw` is a plain relative name: at least one segment, every
/// segment an ordinary name.
///
/// For strings that *name something being created* (a snapshot, a cache
/// directory for a source), where `.` is not path-writing convenience but a lie
/// about what is named. `daily/2026` is accepted; `.`, `daily/.`, `./daily`,
/// `/daily`, `daily/`, `daily//x`, `C:/daily` and `C:daily` are not.
///
/// Judged on the raw string rather than [`std::path::Path::components`], which
/// normalizes `.` away: `"daily/."` iterates as the single plain component
/// `daily` while the joined path still ends in `/.` and resolves to `daily`
/// itself — so a caller that then removes the "new" path removes the parent of
/// everything already inside it.
pub fn validate_plain_name(raw: &str) -> Result<(), String> {
    if raw.is_empty() {
        return Err("it is empty".to_string());
    }
    let rooted = |kind: &str| {
        Err(format!(
            "it starts from {kind}; a name is resolved inside the directory it belongs to, \
             and `Path::join` throws the parent away when the value is rooted"
        ))
    };
    for component in std::path::Path::new(raw).components() {
        match component {
            std::path::Component::Prefix(_) => return rooted("a drive or share"),
            std::path::Component::RootDir => return rooted("a filesystem root"),
            _ => {}
        }
    }
    for segment in raw.split(['/', '\\']) {
        if segment.is_empty() {
            return Err(
                "it has an empty path segment; every segment must name something".to_string(),
            );
        }
        if segment == "." || segment == ".." {
            return Err(format!(
                "the segment '{segment}' is a directory reference, not a name"
            ));
        }
        // Windows reads `C:name` as drive-relative and `name:stream` as an NTFS
        // alternate data stream, and unix parses neither as a prefix — so the
        // shape is refused on every host, keeping a name written on one OS valid
        // on the others rather than only where it happened to be created.
        if segment.contains(':') {
            return Err(format!(
                "the segment '{segment}' contains ':', a drive or data-stream separator on Windows"
            ));
        }
    }
    Ok(())
}

/// A schedule that reads as neither of the two forms cfgd accepts. Carries both
/// attempted interpretations' own errors, so a typo in either form is
/// diagnosable from the message alone.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ScheduleGrammarError(pub String);

/// Validate a backup unit's `schedule`: it must parse as either a
/// [`parse_duration_str`] interval or a `croner` cron expression.
///
/// The ONE grammar behind both the machine's own `spec.backups[].schedule` and
/// the cluster-side `BackupPolicy.spec.units[].schedule`. A policy exists only
/// to set a cadence, so a value the machine's scheduler cannot parse is refused
/// where it is written rather than projecting onto a unit that then silently
/// never fires.
pub fn validate_backup_schedule_grammar(schedule: &str) -> Result<(), ScheduleGrammarError> {
    let duration_err = match parse_duration_str(schedule) {
        Ok(_) => return Ok(()),
        Err(e) => e,
    };
    let cron_err = match schedule.parse::<croner::Cron>() {
        Ok(_) => return Ok(()),
        Err(e) => e,
    };
    Err(ScheduleGrammarError(format!(
        "schedule '{schedule}' is not a valid interval ({duration_err}) and not a valid cron expression ({cron_err})"
    )))
}

/// Validate a backup unit's `name`.
///
/// The name is a directory component (`<state_dir>/backups/<name>/`), a lock
/// filename (`<state_dir>/locks/backup-<name>.lock`), and the key the retention
/// pass prunes by — three roots cfgd creates and later deletes wholesale — so it
/// goes through [`validate_plain_name`], the shared gate for exactly that class.
/// Only the single-component rule is checked here on top: `validate_plain_name`
/// accepts a nested `daily/2026`, which a backup name must not be.
///
/// The ONE grammar behind both the machine's own `spec.backups[].name` and the
/// cluster-side `BackupPolicy.spec.units[].name`: a policy naming a unit no
/// local profile could legally define matches nothing on any machine.
pub fn validate_backup_unit_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("backup name must not be empty or whitespace-only".to_string());
    }
    if name.contains('/') || name.contains('\\') {
        return Err(format!(
            "backup name '{name}' must not contain path separators ('/' or '\\'); it is used as a directory component (<state_dir>/backups/<name>/)"
        ));
    }
    if let Err(why) = validate_plain_name(name) {
        return Err(format!(
            "backup name '{name}' is not usable as a name: {why}; it becomes a directory component (<state_dir>/backups/<name>/) and a lock file (<state_dir>/locks/backup-<name>.lock)"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ONE cadence grammar both `spec.backups[].schedule` and
    /// `BackupPolicy.spec.units[].schedule` answer to; a refusal names both
    /// attempted interpretations so a typo in either form is diagnosable.
    #[test]
    fn a_backup_schedule_is_an_interval_or_a_cron_expression() {
        for good in ["6h", "30s", "90", "0 3 * * *", "*/15 * * * *"] {
            assert!(
                validate_backup_schedule_grammar(good).is_ok(),
                "{good} is a schedule cfgd parses"
            );
        }
        for bad in ["", "  ", "nightly", "0 3 * *", "every day"] {
            let why = validate_backup_schedule_grammar(bad)
                .expect_err("{bad} is neither form")
                .to_string();
            assert!(
                why.contains("not a valid interval") && why.contains("not a valid cron expression"),
                "the refusal names both interpretations: {why}"
            );
        }
    }

    /// The ONE name grammar both sides answer to: a backup name becomes a
    /// directory component and a lock filename, so it is one plain segment.
    #[test]
    fn a_backup_unit_name_is_one_plain_segment() {
        for good in ["dotfiles", "daily-notes", "a.b.c"] {
            assert!(
                validate_backup_unit_name(good).is_ok(),
                "{good} is a usable unit name"
            );
        }
        for bad in ["", "   ", "daily/2026", r"daily\2026", ".", "..", "C:evil"] {
            assert!(
                validate_backup_unit_name(bad).is_err(),
                "{bad} is not a usable unit name"
            );
        }
    }

    #[test]
    fn parse_duration_str_seconds() {
        let d = parse_duration_str("30s").unwrap();
        assert_eq!(d, std::time::Duration::from_secs(30));
    }

    #[test]
    fn parse_duration_str_minutes() {
        let d = parse_duration_str("5m").unwrap();
        assert_eq!(d, std::time::Duration::from_secs(300));
    }

    #[test]
    fn parse_duration_str_hours() {
        let d = parse_duration_str("1h").unwrap();
        assert_eq!(d, std::time::Duration::from_secs(3600));
    }

    #[test]
    fn parse_duration_str_plain_seconds() {
        let d = parse_duration_str("60").unwrap();
        assert_eq!(d, std::time::Duration::from_secs(60));
    }

    #[test]
    fn parse_duration_str_whitespace() {
        let d = parse_duration_str(" 10 s ").unwrap();
        assert_eq!(d, std::time::Duration::from_secs(10));
    }

    #[test]
    fn parse_duration_str_days() {
        let d = parse_duration_str("30d").unwrap();
        assert_eq!(d, std::time::Duration::from_secs(30 * 86400));
    }

    #[test]
    fn parse_duration_str_invalid() {
        assert!(
            parse_duration_str("abc")
                .unwrap_err()
                .contains("invalid timeout"),
            "bare letters should fail with a useful message"
        );
        assert!(
            parse_duration_str("")
                .unwrap_err()
                .contains("invalid timeout"),
            "empty string should fail"
        );
        assert!(
            parse_duration_str("xs")
                .unwrap_err()
                .contains("invalid timeout"),
            "non-numeric prefix should fail"
        );
    }

    #[test]
    fn parse_duration_str_zero() {
        assert_eq!(
            parse_duration_str("0s").unwrap(),
            std::time::Duration::from_secs(0)
        );
        assert_eq!(
            parse_duration_str("0").unwrap(),
            std::time::Duration::from_secs(0)
        );
    }

    #[test]
    fn parse_duration_str_negative() {
        assert!(
            parse_duration_str("-5s").is_err(),
            "negative durations should be rejected"
        );
    }

    #[test]
    fn validate_plain_name_accepts_ordinary_names() {
        for candidate in ["snapshot", "daily/2026", "a.b.c", "..hidden", "x..y"] {
            assert!(
                validate_plain_name(candidate).is_ok(),
                "'{candidate}' should be a usable name"
            );
        }
    }

    #[test]
    fn validate_plain_name_rejects_every_directory_reference() {
        // `daily/.` is the one `Path::components()` cannot see: it normalizes to the
        // single component `daily` while the joined path still resolves to `daily`
        // itself rather than to something new inside it.
        for candidate in [".", "..", "daily/.", "./daily", "a/../b", "/daily", "a//b"] {
            assert!(
                validate_plain_name(candidate).is_err(),
                "'{candidate}' does not name something new and must be rejected"
            );
        }
        assert!(validate_plain_name("").is_err());
        // Windows separators are judged too — the check runs before any `Path` parse,
        // where a `\` would otherwise be an ordinary character on unix.
        assert!(validate_plain_name(r"daily\.").is_err());
    }

    #[test]
    fn validate_plain_name_rejects_a_rooted_value_on_every_host() {
        // `Path::join` discards the base for a rooted right-hand side, so any of
        // these would silently relocate whatever the caller was building. Windows
        // shapes are rejected on unix too: the name may have been written into
        // shared state by a Windows host.
        for candidate in [
            "/abs",
            r"\abs",
            "C:/evil",
            r"C:\evil",
            "C:evil",
            r"\\server\share",
        ] {
            assert!(
                validate_plain_name(candidate).is_err(),
                "'{candidate}' is rooted and must not be accepted as a name"
            );
        }
        // A colon anywhere is an NTFS alternate-data-stream selector, not a name.
        assert!(validate_plain_name("notes.txt:hidden").is_err());
        assert!(validate_plain_name("daily/C:evil").is_err());
    }

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

    /// Every label-bearing type this crate owns, with the `(canonical token,
    /// display label)` pairs read off its own `ALL` — so a new VARIANT is
    /// covered by construction. The type list is the only hand-written half,
    /// and the walk below checks it against the source.
    fn labelled_types() -> Vec<(&'static str, Vec<(&'static str, &'static str)>)> {
        vec![
            (
                "FileStrategy",
                FileStrategy::ALL
                    .iter()
                    .map(|v| (v.as_str(), v.method_label()))
                    .collect(),
            ),
            (
                "ScheduleOwner",
                ScheduleOwner::ALL
                    .iter()
                    .map(|v| (v.as_str(), v.label()))
                    .collect(),
            ),
        ]
    }

    /// A display label is the ASCII-lowercase of the canonical token beside it,
    /// on every label-bearing type this crate owns: a hand-written arm
    /// returning anything else compiles, and one that reads `Local` where the
    /// listing prints `local` would make the two spellings of one value drift.
    /// The variant population comes from each type's `ALL`; the TYPE population
    /// is read back off the source, so a third label-bearing type cannot be
    /// invisible to this walk the way a hand-listed pair of enums would let it
    /// be.
    #[test]
    fn every_display_label_is_the_lowercase_of_its_canonical_token() {
        let table = labelled_types();
        for (ty, pairs) in &table {
            assert!(!pairs.is_empty(), "{ty} states no variants");
            for (token, label) in pairs {
                assert_eq!(
                    *label,
                    token.to_ascii_lowercase(),
                    "{ty}::{token}'s label is not its token lowercased"
                );
            }
        }

        // The trailing test module carries these very literals, so the walk
        // reads the production region alone.
        let production = include_str!("lib.rs")
            .split("\n#[cfg(test)]")
            .next()
            .expect("a source has a first region");
        let mut current = None;
        let mut sites: Vec<&str> = Vec::new();
        for line in production.lines() {
            if let Some(rest) = line.strip_prefix("impl ") {
                current = rest.split_whitespace().next();
            }
            if line.contains("pub fn label(") || line.contains("pub fn method_label(") {
                sites.push(current.unwrap_or_else(|| panic!("a label fn outside an impl: {line}")));
            }
        }
        assert!(
            sites.len() >= 2,
            "the walk no longer reaches the crate's label fns — it found {sites:?}"
        );
        let listed: Vec<&str> = table.iter().map(|(ty, _)| *ty).collect();
        for site in &sites {
            assert!(
                listed.contains(site),
                "{site} states a display label no walk checks; add it to `labelled_types`"
            );
        }
        for ty in &listed {
            assert!(
                sites.contains(ty),
                "{ty} is listed but states no display label in this crate"
            );
        }
    }

    #[test]
    fn schedule_owner_defaults_to_cluster_and_parses_case_insensitively() {
        let absent: BackupSpec =
            serde_yaml::from_str("name: db\nsource: /var/lib/db\n").expect("backup spec parses");
        assert_eq!(absent.schedule_owner, ScheduleOwner::Cluster);

        let pinned: BackupSpec =
            serde_yaml::from_str("name: db\nsource: /var/lib/db\nscheduleOwner: LOCAL\n")
                .expect("a shouted token parses");
        assert_eq!(pinned.schedule_owner, ScheduleOwner::Local);

        let rendered = serde_yaml::to_string(&pinned).expect("serialize");
        assert!(
            rendered.contains("scheduleOwner: Local\n"),
            "the canonical wire spelling survives the round trip: {rendered}"
        );
        assert_eq!(pinned.schedule_owner.label(), "local");
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
