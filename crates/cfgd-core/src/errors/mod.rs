// Domain error types — thiserror for library errors, anyhow only at CLI boundary

use std::path::PathBuf;

use crate::PathDisplayExt;

pub type Result<T> = std::result::Result<T, CfgdError>;

/// Render a path list as `'a', 'b', 'c'` (posix separators) for single-line
/// error messages that must name every candidate.
fn join_quoted_posix(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| format!("'{}'", p.posix()))
        .collect::<Vec<_>>()
        .join(", ")
}

// Top-level variants print `"<category>: <inner>"` because `{0}` expands the
// inner error's Display once. `main.rs` formats with `{}`, which emits this
// single-layer message. Do NOT switch `main.rs` to `{:#}` — that also walks
// `source()` (via `#[from]`) and would duplicate the inner text. The
// Composition variant uses `#[source]` (not `#[from]`) because a manual
// `From<CompositionError>` impl exists for error-context wrapping.
#[derive(Debug, thiserror::Error)]
pub enum CfgdError {
    #[error("config error: {0}")]
    Config(#[from] ConfigError),

    #[error("file error: {0}")]
    File(#[from] FileError),

    #[error("package error: {0}")]
    Package(#[from] PackageError),

    #[error("secret error: {0}")]
    Secret(#[from] SecretError),

    #[error("state error: {0}")]
    State(#[from] StateError),

    #[error("daemon error: {0}")]
    Daemon(#[from] DaemonError),

    #[error("source error: {0}")]
    Source(#[from] SourceError),

    #[error("composition error: {0}")]
    Composition(#[source] Box<CompositionError>),

    #[error("upgrade error: {0}")]
    Upgrade(#[from] UpgradeError),

    #[error("module error: {0}")]
    Module(#[from] ModuleError),

    #[error("generate error: {0}")]
    Generate(#[from] GenerateError),

    #[error("oci error: {0}")]
    Oci(#[from] OciError),

    #[error("skill error: {0}")]
    Skill(#[from] SkillError),

    #[error("backup error: {0}")]
    Backup(#[from] BackupError),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl CfgdError {
    /// A stable, machine-readable name for the top-level variant — the
    /// domain a structured (`-o json`) consumer can route a failure on
    /// without parsing the human message. Snake_case to match every other
    /// `error_kind` string the CLI boundary already emits (`not_found`,
    /// `already_exists`, …).
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Config(_) => "config",
            Self::File(_) => "file",
            Self::Package(_) => "package",
            Self::Secret(_) => "secret",
            Self::State(_) => "state",
            Self::Daemon(_) => "daemon",
            Self::Source(_) => "source",
            Self::Composition(_) => "composition",
            Self::Upgrade(_) => "upgrade",
            Self::Module(_) => "module",
            Self::Generate(_) => "generate",
            Self::Oci(_) => "oci",
            Self::Skill(_) => "skill",
            Self::Backup(_) => "backup",
            Self::Io(_) => "io",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config file not found: {path}")]
    NotFound { path: PathBuf },

    #[error(
        "cannot resolve home directory (HOME unset) to locate config at {path}; set HOME or pass --config <path>"
    )]
    HomeUnresolved { path: PathBuf },

    #[error("invalid config: {message}")]
    Invalid { message: String },

    #[error(
        "unsupported apiVersion {found:?}; this build supports {}",
        crate::API_VERSION
    )]
    UnsupportedApiVersion { found: String },

    #[error("circular profile inheritance: {chain:?}")]
    CircularInheritance { chain: Vec<String> },

    #[error("profile not found: {name}")]
    ProfileNotFound { name: String },

    #[error("key '{key}' not found in config")]
    KeyNotFound { key: String },

    #[error(
        "ambiguous profile '{name}': multiple forms exist ({forms}) — delete or rename one of them (the canonical form is '{name}/profile.yaml')",
        forms = join_quoted_posix(.paths)
    )]
    AmbiguousProfile { name: String, paths: Vec<PathBuf> },

    #[error("yaml parse error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    #[error("toml parse error: {0}")]
    Toml(#[from] toml::de::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum FileError {
    #[error("source file not found: {path}")]
    SourceNotFound { path: PathBuf },

    #[error("target path not writable: {path}")]
    TargetNotWritable { path: PathBuf },

    #[error("template rendering failed for {path}: {message}")]
    TemplateError { path: PathBuf, message: String },

    #[error("permission denied setting mode {mode:#o} on {path}")]
    PermissionDenied { path: PathBuf, mode: u32 },

    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error(
        "file conflict: {target} is targeted by both '{source_a}' and '{source_b}' with different content"
    )]
    Conflict {
        target: PathBuf,
        source_a: String,
        source_b: String,
    },

    #[error("source file changed between plan and apply: {path}")]
    SourceChanged { path: PathBuf },

    #[error("path {path} escapes root directory {root}")]
    PathTraversal { path: PathBuf, root: PathBuf },

    #[error(
        "source file '{path}' must be encrypted with '{backend}' but appears to be unencrypted"
    )]
    NotEncrypted { path: PathBuf, backend: String },

    #[error("unknown encryption backend '{backend}' — supported: sops, age")]
    UnknownEncryptionBackend { backend: String },

    #[error(
        "encryption mode 'Always' is incompatible with strategy '{strategy}' for '{path}' — use Copy or Template instead"
    )]
    EncryptionStrategyIncompatible { path: PathBuf, strategy: String },

    #[error("strategy 'Patch' for {path} requires a 'patch' block")]
    PatchBlockMissing { path: PathBuf },

    #[error(
        "cannot infer a patch format from the extension of {path} — set 'patch.format' explicitly (ini, json, yaml, toml)"
    )]
    PatchFormatUnknown { path: PathBuf },

    #[error("patch target {path} is not valid {format}: {message}")]
    PatchParse {
        path: PathBuf,
        format: String,
        message: String,
    },

    #[error("failed to serialize the patched {format} content for {path}: {message}")]
    PatchSerialize {
        path: PathBuf,
        format: String,
        message: String,
    },

    #[error("patch 'ensure' for {path} ({format}) is invalid: {message}")]
    PatchEnsureShape {
        path: PathBuf,
        format: String,
        message: String,
    },

    #[error("patch script '{script}' failed for {path}: {message}")]
    PatchScriptFailed {
        path: PathBuf,
        script: String,
        message: String,
    },

    #[error("patch block for {path} must set exactly one of 'ensure' or 'script'")]
    PatchSpecInvalid { path: PathBuf },

    #[error(
        "patch script for {path} is blocked: source '{source_name}' is not allowed to run scripts (constraints.noScripts); set subscription.allowScripts: true to opt in"
    )]
    PatchScriptBlocked { path: PathBuf, source_name: String },
}

#[derive(Debug, thiserror::Error)]
pub enum PackageError {
    // The manager IS registered, and two paths reach it: a `--phase packages`
    // run that bypassed the `Prerequisites` phase, and an unfiltered run whose
    // provision node FAILED — the install is not a dependent of that node, so
    // it is still dispatched and still asks. Naming a filter is therefore a
    // guess, and it read as one ("or drop --phase" against a command line
    // carrying no --phase); what holds for both is the phase that owns
    // provisioning, which is where the filtered run's recovery and the failed
    // run's reason both live.
    #[error(
        "{manager} is not provisioned — provisioning is the Prerequisites phase's: `cfgd apply --phase prerequisites.managers`"
    )]
    ManagerNotAvailable { manager: String },

    #[error("{manager} install failed: {message}")]
    InstallFailed { manager: String, message: String },

    #[error("{manager} uninstall failed: {message}")]
    UninstallFailed { manager: String, message: String },

    #[error("{manager} failed to list installed packages: {message}")]
    ListFailed { manager: String, message: String },

    #[error("{manager} command failed: {source}")]
    CommandFailed {
        manager: String,
        source: std::io::Error,
    },

    #[error("{manager} bootstrap failed: {message}")]
    BootstrapFailed { manager: String, message: String },

    // The manager is not registered at all — no phase can provision a name
    // that does not exist, so this carries no phase-run guidance (unlike
    // `ManagerNotAvailable`, whose recovery is always the `Prerequisites`
    // phase).
    #[error("package manager '{manager}' not available")]
    ManagerNotFound { manager: String },

    // A worker thread that unwinds without reporting would leave the apply
    // coordinator waiting on a message that can no longer arrive, so a lane
    // catches its own unwind and fails the action instead of hanging the run.
    #[error("{manager} package work panicked")]
    LanePanicked { manager: String },

    // A coordinator invariant failure, not a package failure: `pick_next`
    // left this action `Waiting` with nothing running and nothing left to
    // dispatch, so no lane will ever run it. Reported as a failed action
    // (rather than silently dropped) so the run's status — and its exit
    // code — cannot read `Success` over a shortfall it walked away from.
    #[error("{manager} dispatch stalled — no lane ever became available for this action")]
    LaneStalled { manager: String },

    // The coordinator's inbox disconnected with work still outstanding: every
    // worker handle was dropped without a `Finished` message ever landing,
    // which a lane's own panic guard cannot see (a worker the OS killed, a
    // scoped thread that never started). Reported per outstanding action for
    // the same reason `LaneStalled` is — an action neither the exit code nor
    // the tree ever hears about is a shortfall the run would walk away from
    // reporting success.
    #[error("{manager} lane ended without reporting — this action never ran to completion")]
    LaneLost { manager: String },

    // A `Prerequisites` node whose dependency failed. It never ran: what it was
    // waiting to be handed does not exist, so running it anyway would be the
    // silent bootstrap that phase exists to replace. Named after the ROOT
    // failure rather than the nearest link, so the line points at what to fix.
    #[error("did not run — {dependency} failed earlier in this phase")]
    DependencyFailed { dependency: String },
}

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("sops not found — install: https://github.com/getsops/sops#install")]
    SopsNotFound,

    #[error("sops encryption failed for {path}: {message}")]
    EncryptionFailed { path: PathBuf, message: String },

    #[error("sops decryption failed for {path}: {message}")]
    DecryptionFailed { path: PathBuf, message: String },

    #[error("secret provider '{provider}' not available — {hint}")]
    ProviderNotAvailable { provider: String, hint: String },

    #[error("secret reference unresolvable: {reference}")]
    UnresolvableRef { reference: String },

    #[error("age key not found at {path}")]
    AgeKeyNotFound { path: PathBuf },
}

/// Failures of a declarative backup (`spec.backups[]`).
///
/// Most of these are recorded as the `error` of a failed run rather than
/// returned to the caller — see [`crate::backup::run_backup`]. They are typed
/// anyway so every surface (recorded row, terminal line, future `-o json`)
/// renders the same wording for the same failure.
#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("backup '{name}': source does not exist: {}", .path.posix())]
    SourceMissing { name: String, path: PathBuf },

    #[error("backup '{name}': cannot read source {}: {source}", .path.posix())]
    SourceUnreadable {
        name: String,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("backup '{name}': cannot write snapshot to {}: {source}", .path.posix())]
    CopyFailed {
        name: String,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(
        "backup '{name}': namePattern rendered the unusable snapshot name '{rendered}' ({message}); \
         {{filename}} was '{source_filename}' — if the filename itself is what makes the name \
         unusable, set an explicit namePattern that does not use it"
    )]
    InvalidSnapshotName {
        name: String,
        rendered: String,
        source_filename: String,
        message: String,
    },

    #[error(
        "backup '{name}': destination {} is inside source {} — every snapshot would be copied into the next one, without end; move the destination outside the source",
        .destination.posix(), .source_path.posix()
    )]
    DestinationInsideSource {
        name: String,
        source_path: PathBuf,
        destination: PathBuf,
    },

    #[error(
        "backup '{name}': the snapshot path {} collides with source {} — taking it would destroy the data being backed up; change namePattern or destination",
        .snapshot.posix(), .source_path.posix()
    )]
    SnapshotCollidesWithSource {
        name: String,
        source_path: PathBuf,
        snapshot: PathBuf,
    },

    #[error("backup '{name}': {phase} hook failed: {message}")]
    HookFailed {
        name: String,
        phase: &'static str,
        message: String,
    },

    /// `cfgd backup run <name>` (or the daemon) named a backup that is not in
    /// the active profile's `spec.backups[]`. `valid` lists every declared
    /// name so the caller can render "did you mean" without a second lookup.
    #[error(
        "backup '{name}' not found{}",
        if .valid.is_empty() {
            String::new()
        } else {
            format!(" — valid backups: {}", .valid.join(", "))
        }
    )]
    UnknownName { name: String, valid: Vec<String> },

    /// Another process (or another thread of this one) is already running this
    /// exact backup. Reported instead of waiting: the engine's staging path,
    /// destination replace, and retention prune all assume a single writer per
    /// unit, and two runs of one unit a second apart produce a torn snapshot
    /// recorded as a success.
    #[error(
        "backup '{name}' is already running ({holder}); wait for it to finish or stop the other run"
    )]
    Busy { name: String, holder: String },

    /// `cfgd backup restore <name>` on a unit that has never produced a
    /// snapshot. Distinct from [`BackupError::SnapshotNotFound`]: there is no
    /// list of alternatives to offer, only a run to take first.
    #[error("backup '{name}': no snapshots to restore — run `cfgd backup run {name}` first")]
    NoSnapshots { name: String },

    /// `--at` named a snapshot this unit has no record of. `available` lists
    /// every restorable snapshot, newest first, so the caller can render the
    /// alternatives without a second lookup — the same shape
    /// [`BackupError::UnknownName`] uses for backup names.
    #[error(
        "backup '{name}': no snapshot matches '{requested}'{}",
        if .available.is_empty() {
            String::new()
        } else {
            format!(" — available snapshots: {}", .available.join(", "))
        }
    )]
    SnapshotNotFound {
        name: String,
        requested: String,
        available: Vec<String>,
    },

    /// `--at` was given a timestamp fragment that more than one snapshot name
    /// contains. Refused rather than resolved to the newest match: a restore
    /// overwrites live data, and guessing which snapshot the operator meant is
    /// the one place that must not be guessed.
    #[error(
        "backup '{name}': '{requested}' matches {} snapshots ({}); pass the full snapshot name",
        .matches.len(), .matches.join(", ")
    )]
    AmbiguousSnapshot {
        name: String,
        requested: String,
        matches: Vec<String>,
    },

    /// The selected snapshot vanished between being listed and being staged —
    /// a concurrent prune, or a hand-deleted destination.
    #[error(
        "backup '{name}': snapshot {} is no longer on disk; it may have been pruned since it was listed",
        .path.posix()
    )]
    SnapshotMissing { name: String, path: PathBuf },

    #[error("backup '{name}': cannot stage snapshot {} for restore: {source}", .path.posix())]
    StagingFailed {
        name: String,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("backup '{name}': cannot restore into {}: {source}", .path.posix())]
    RestoreFailed {
        name: String,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The snapshot and the restore target disagree about what they are. A
    /// file snapshot published over a directory target would delete the whole
    /// directory on the way to the rename, which is well past overlay
    /// semantics, so both directions are refused before anything is touched.
    #[error(
        "backup '{name}': the snapshot is a {snapshot_kind} but the restore target {} is a {target_kind}; \
         remove or rename the target, or restore elsewhere with --to",
        .target.posix()
    )]
    RestoreKindMismatch {
        name: String,
        target: PathBuf,
        snapshot_kind: &'static str,
        target_kind: &'static str,
    },

    /// The safety backup taken immediately before a restore-to-source did not
    /// produce a snapshot. The restore is abandoned: overwriting live data
    /// whose current contents were NOT captured is the failure mode the safety
    /// backup exists to prevent.
    #[error(
        "backup '{name}': the safety backup of the current source failed ({message}); \
         refusing to overwrite data that is not backed up — fix the failure, or pass --to to restore elsewhere"
    )]
    SafetyBackupFailed { name: String, message: String },

    /// A fatal failure aborted a restore while the unit's `postBackup` hook
    /// ALSO failed on the way out. Carried as one error because the abort is
    /// the primary condition, but a structured-output consumer would never
    /// see a hook failure reported only as a stderr status line.
    #[error("{fatal}; additionally: {post_message}")]
    RestoreAbortHookFailed {
        #[source]
        fatal: Box<CfgdError>,
        post_message: String,
    },

    /// A `--to` that points at (or into) the unit's own snapshot destination.
    /// Restoring there would overwrite the snapshot store with one of its own
    /// snapshots and desynchronize it from the run records retention walks.
    #[error(
        "backup '{name}': restore target {} is inside the snapshot destination {}; \
         restoring there would overwrite the snapshot store",
        .target.posix(), .destination.posix()
    )]
    RestoreTargetInsideDestination {
        name: String,
        target: PathBuf,
        destination: PathBuf,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("no apply found with ID {apply_id}")]
    ApplyNotFound { apply_id: i64 },

    #[error("state database error: {0}")]
    Database(String),

    #[error("migration failed: {message}")]
    MigrationFailed { message: String },

    #[error("state directory not writable: {path}")]
    DirectoryNotWritable { path: PathBuf },

    #[error("state filesystem I/O failed at {path}: {source}")]
    FilesystemIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("state serialization failed ({context}): {source}")]
    Serialize {
        context: &'static str,
        #[source]
        source: serde_json::Error,
    },

    #[error("apply lock held by another process: {holder}")]
    ApplyLockHeld { holder: String },

    // The lock-acquire identity re-check spent its budget without ever
    // confirming the locked file was still the one the path names. Nobody is
    // known to hold anything, so this deliberately names the file rather than
    // a holder: sending the operator after a PID would be a lie, and the
    // source-lock path must not read the failure as contention.
    #[error(
        "could not safely acquire the lock at {path}: the lock file kept changing underneath the acquire"
    )]
    LockFileUnstable { path: PathBuf },

    // Every SQLite access from a concurrent install lane is a message to the
    // coordinator, which owns the one connection; this is that message failing
    // to make the round trip.
    #[error("package state unreachable from an install lane: {reason}")]
    LaneUnreachable { reason: String },
}

impl From<rusqlite::Error> for StateError {
    fn from(e: rusqlite::Error) -> Self {
        StateError::Database(e.to_string())
    }
}

impl From<CompositionError> for CfgdError {
    fn from(e: CompositionError) -> Self {
        CfgdError::Composition(Box::new(e))
    }
}

impl From<rusqlite::Error> for CfgdError {
    fn from(e: rusqlite::Error) -> Self {
        CfgdError::State(StateError::Database(e.to_string()))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("source '{name}' not found")]
    NotFound { name: String },

    #[error("failed to fetch source '{name}': {message}")]
    FetchFailed { name: String, message: String },

    #[error("invalid ConfigSource manifest in '{name}': {message}")]
    InvalidManifest { name: String, message: String },

    #[error("no git ref matched pin '{pin}' for source '{name}'{}", .available.as_ref().map(|a| format!(" (available tags: {a})")).unwrap_or_default())]
    PinRefNotFound {
        name: String,
        pin: String,
        available: Option<String>,
    },

    #[error("source '{name}' provides neither profiles nor modules")]
    EmptyProvides { name: String },

    #[error("profile '{profile}' not found in source '{name}'")]
    ProfileNotFound { name: String, profile: String },

    #[error("source cache error: {message}")]
    CacheError { message: String },

    #[error("git error for source '{name}': {message}")]
    GitError { name: String, message: String },

    #[error("signature verification failed for source '{name}': {message}")]
    SignatureVerificationFailed { name: String, message: String },
}

#[derive(Debug, thiserror::Error)]
pub enum CompositionError {
    #[error("cannot override locked resource '{resource}' from source '{source_name}'")]
    LockedResource {
        source_name: String,
        resource: String,
    },

    #[error("cannot remove required resource '{resource}' from source '{source_name}'")]
    RequiredResource {
        source_name: String,
        resource: String,
    },

    #[error("path '{path}' not in allowed paths for source '{source_name}'")]
    PathNotAllowed { source_name: String, path: String },

    #[error("invalid overrides for source '{source_name}': {message}")]
    InvalidOverrides {
        source_name: String,
        message: String,
    },

    #[error(
        "unknown reject key '{key}' for source '{source_name}' (allowed: packages, env, aliases, modules)"
    )]
    InvalidReject { source_name: String, key: String },

    #[error(
        "source '{source_name}' carries {kind}, but it is not allowed to run scripts (set subscription.allowScripts: true to opt in, or relax the source's constraints.noScripts)"
    )]
    ScriptsNotAllowed { source_name: String, kind: String },

    #[error(
        "required source '{source_name}' is not available (not synced or failed to load); fix the source or set its sync.required to false"
    )]
    RequiredSourceUnavailable { source_name: String },

    #[error("source '{source_name}' template attempted to access local variable '{variable}'")]
    TemplateSandboxViolation {
        source_name: String,
        variable: String,
    },

    #[error(
        "source '{source_name}' attempted to modify system setting '{setting}' without permission"
    )]
    SystemChangeNotAllowed {
        source_name: String,
        setting: String,
    },

    #[error("conflict on '{resource}' between sources: {source_names:?}")]
    UnresolvableConflict {
        resource: String,
        source_names: Vec<String>,
    },

    #[error(
        "file '{path}' matches required-encryption target '{pattern}' in source '{source_name}' but has no encryption block"
    )]
    EncryptionRequired {
        source_name: String,
        path: String,
        pattern: String,
    },

    #[error(
        "file '{path}' matches required-encryption target '{pattern}' in source '{source_name}' but uses backend '{actual_backend}' instead of required '{required_backend}'"
    )]
    EncryptionBackendMismatch {
        source_name: String,
        path: String,
        pattern: String,
        actual_backend: String,
        required_backend: String,
    },

    #[error(
        "file '{path}' matches required-encryption target '{pattern}' in source '{source_name}' but uses mode '{actual_mode}' instead of required '{required_mode}'"
    )]
    EncryptionModeMismatch {
        source_name: String,
        path: String,
        pattern: String,
        actual_mode: String,
        required_mode: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum UpgradeError {
    #[error("failed to query GitHub releases: {message}")]
    ApiError { message: String },

    #[error("no release found for {os}/{arch}")]
    NoAsset { os: String, arch: String },

    #[error("download failed: {message}")]
    DownloadFailed { message: String },

    #[error("checksum verification failed for {file}")]
    ChecksumMismatch { file: String },

    #[error("no checksum (.sha256) published for {file}")]
    ChecksumMissing { file: String },

    #[error("published checksum file was empty or malformed")]
    ChecksumsEmpty,

    #[error("failed to install binary: {message}")]
    InstallFailed { message: String },

    #[error("version parse error: {message}")]
    VersionParse { message: String },

    #[error(
        "strict cosign verification required but unavailable: {reason} — re-run without --require-cosign / unset CFGD_REQUIRE_COSIGN to allow SHA256-only fallback"
    )]
    CosignRequired { reason: String },
}

#[derive(Debug, thiserror::Error)]
pub enum ModuleError {
    #[error("module not found: {name}")]
    NotFound { name: String },

    #[error(
        "module '{name}' is declared by source '{source_name}' (provides.modules) but its body is missing from that source — the publisher must add modules/{name}/module.yaml, or run 'cfgd source update {source_name}'"
    )]
    OfferedButMissing { name: String, source_name: String },

    #[error("module registry not found: {name}")]
    RegistryNotFound { name: String },

    #[error("module dependency cycle: {chain:?}")]
    DependencyCycle { chain: Vec<String> },

    #[error("module '{module}' depends on '{dependency}' which is not available")]
    MissingDependency { module: String, dependency: String },

    #[error(
        "module '{module}' depends on '{dependency}', which is skipped on this platform ({dependency} requires: {platforms})"
    )]
    DependencyPlatformSkipped {
        module: String,
        dependency: String,
        platforms: String,
    },

    #[error(
        "package '{package}' in module '{module}' cannot be resolved: no available manager satisfies the requirements (minVersion: {min_version})"
    )]
    UnresolvablePackage {
        module: String,
        package: String,
        min_version: String,
    },

    #[error("failed to fetch git source for module '{module}': {url}: {message}")]
    GitFetchFailed {
        module: String,
        url: String,
        message: String,
    },

    #[error("module '{name}' has invalid spec: {message}")]
    InvalidSpec { name: String, message: String },

    #[error(
        "module '{module}' delivered by source '{source_name}' carries {kind}, but that source is not allowed to run scripts (set subscription.allowScripts: true to opt in, or relax the source's constraints.noScripts)"
    )]
    ScriptsNotAllowed {
        source_name: String,
        module: String,
        kind: String,
    },

    #[error(
        "lockfile integrity check failed for module '{name}': expected {expected}, got {actual}"
    )]
    IntegrityMismatch {
        name: String,
        expected: String,
        actual: String,
    },

    #[error(
        "remote module '{name}' requires a pinned ref (tag or commit) — branch tracking is not allowed for security"
    )]
    UnpinnedRemoteModule { name: String },

    #[error("module source fetch failed for '{url}': {message}")]
    SourceFetchFailed { url: String, message: String },
}

#[derive(Debug, thiserror::Error)]
pub enum GenerateError {
    #[error("validation failed: {message}")]
    ValidationFailed { message: String },

    #[error("file access denied: {path} — {reason}")]
    FileAccessDenied { path: PathBuf, reason: String },

    #[error("AI provider error: {message}")]
    ProviderError { message: String },

    #[error("API key not found in environment variable '{env_var}'")]
    ApiKeyNotFound { env_var: String },
}

#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    #[error(
        "unknown provider '{name}'{}",
        if .valid.is_empty() {
            String::new()
        } else {
            format!(" — valid providers: {}", .valid.join(", "))
        }
    )]
    UnknownProvider { name: String, valid: Vec<String> },

    #[error("failed to render skill for provider '{provider}': {message}")]
    Render { provider: String, message: String },

    #[error("provider detection failed for '{provider}': {message}")]
    Detect { provider: String, message: String },

    #[error("failed to write skill file: {0}")]
    Write(#[source] std::io::Error),

    #[error("failed to acquire skill-file lock: {0}")]
    Lock(#[source] Box<CfgdError>),
}

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("daemon already running (pid {pid})")]
    AlreadyRunning { pid: u32 },

    #[error("health socket unavailable: {message}")]
    HealthSocketError { message: String },

    #[error("service install failed: {message}")]
    ServiceInstallFailed { message: String },

    #[error("service error: {message}")]
    ServiceError { message: String },

    #[error("watch error: {message}")]
    WatchError { message: String },
}

#[derive(Debug, thiserror::Error)]
pub enum OciError {
    #[error("invalid OCI reference: {reference}")]
    InvalidReference { reference: String },

    #[error("registry authentication failed for {registry}: {message}")]
    AuthFailed { registry: String, message: String },

    #[error("registry request failed: {message}")]
    RequestFailed { message: String },

    #[error("blob upload failed for {digest}: {message}")]
    BlobUploadFailed { digest: String, message: String },

    #[error("manifest push failed: {message}")]
    ManifestPushFailed { message: String },

    #[error("manifest not found: {reference}")]
    ManifestNotFound { reference: String },

    #[error("blob not found: {digest}")]
    BlobNotFound { digest: String },

    #[error("module.yaml not found in {dir}")]
    ModuleYamlNotFound { dir: PathBuf },

    #[error("archive error: {message}")]
    ArchiveError { message: String },

    #[error("build error: {message}")]
    BuildError { message: String },

    #[error("signing error: {message}")]
    SigningError { message: String },

    #[error("signature verification failed for {reference}: {message}")]
    VerificationFailed { reference: String, message: String },

    #[error("attestation error: {message}")]
    AttestationError { message: String },

    #[error("{tool} not found — install it or add it to PATH")]
    ToolNotFound { tool: String },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Table-driven: every `From<SubError> for CfgdError` variant in one test.
    #[test]
    fn all_sub_errors_convert_to_cfgd_error() {
        let cases: Vec<(CfgdError, &str)> = vec![
            (
                ConfigError::ProfileNotFound {
                    name: "test".into(),
                }
                .into(),
                "test",
            ),
            (
                SourceError::NotFound {
                    name: "acme".into(),
                }
                .into(),
                "acme",
            ),
            (
                CompositionError::LockedResource {
                    source_name: "acme".into(),
                    resource: "~/.config/security.yaml".into(),
                }
                .into(),
                "locked",
            ),
            (
                UpgradeError::ChecksumMismatch {
                    file: "cfgd-0.2.0-linux-x86_64.tar.gz".into(),
                }
                .into(),
                "checksum",
            ),
            (
                ModuleError::NotFound {
                    name: "nvim".into(),
                }
                .into(),
                "nvim",
            ),
            (
                GenerateError::ValidationFailed {
                    message: "missing apiVersion".into(),
                }
                .into(),
                "missing apiVersion",
            ),
            (
                std::io::Error::new(std::io::ErrorKind::NotFound, "file missing").into(),
                "file missing",
            ),
        ];
        for (cfgd_err, needle) in &cases {
            assert!(
                cfgd_err.to_string().contains(needle),
                "expected '{}' in: {}",
                needle,
                cfgd_err,
            );
        }
    }

    #[test]
    fn rusqlite_error_converts_to_state_and_cfgd_errors() {
        let state_err: StateError = rusqlite::Error::QueryReturnedNoRows.into();
        assert!(
            matches!(state_err, StateError::Database(_)),
            "rusqlite error must map to StateError::Database",
        );

        let cfgd_err: CfgdError = rusqlite::Error::QueryReturnedNoRows.into();
        assert!(
            matches!(cfgd_err, CfgdError::State(StateError::Database(_))),
            "rusqlite error must map to CfgdError::State(Database)",
        );
    }

    #[test]
    fn kind_names_the_top_level_variant_not_the_literal_word_error() {
        let config_err: CfgdError = ConfigError::ProfileNotFound {
            name: "work".into(),
        }
        .into();
        assert_eq!(config_err.kind(), "config");

        let source_err: CfgdError = SourceError::NotFound {
            name: "acme".into(),
        }
        .into();
        assert_eq!(source_err.kind(), "source");

        let io_err: CfgdError = std::io::Error::new(std::io::ErrorKind::NotFound, "gone").into();
        assert_eq!(io_err.kind(), "io");
    }
}
