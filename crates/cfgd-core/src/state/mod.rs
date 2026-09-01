use std::cell::Cell;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::Scope;
use crate::errors::{Result, StateError};

mod applies;
mod backup_runs;
mod backups;
mod bootstrap;
mod compliance;
mod decisions;
mod drift;
mod journal;
mod managed;
pub use managed::HashRefresh;
mod modules;
mod package_prefix;
mod pending_config;
mod sources;
mod types;

pub use decisions::RESOLUTION_AUTO_ACCEPTED;
pub use pending_config::{
    PENDING_CONFIG_FILENAME, clear_pending_server_config, load_pending_server_config,
    save_pending_server_config,
};
pub use sources::ConfigSourceUpsert;
pub use types::{
    ApplyRecord, ApplyStatus, ApplySummary, BackupRunDraft, BackupRunRecord, BackupRunStatus,
    ComplianceHistoryRow, ConfigSourceRecord, DriftEvent, ENV_SESSION_RESOURCE_ID,
    FileBackupRecord, JournalEntry, MODULE_STATUS_ERROR, MODULE_STATUS_INSTALLED, ManagedResource,
    ModuleFileRecord, ModuleStateRecord, PendingDecision, SOURCE_STATUS_ACTIVE,
    SOURCE_STATUS_ERROR, SourceConfigHash, SourceConflictRecord, backup_run_status_display,
    module_status_display, source_status_display,
};

/// Canonical state DB filename. The single source of truth so the default and
/// explicit-`--state-dir`/`CFGD_STATE_DIR` paths can never diverge onto sibling
/// files (the divergence silently read an empty DB and reported "no history").
pub const STATE_DB_FILENAME: &str = "state.db";

const MIGRATIONS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS applies (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        timestamp TEXT NOT NULL,
        profile TEXT NOT NULL,
        plan_hash TEXT NOT NULL,
        status TEXT NOT NULL,
        summary TEXT
    );

    CREATE TABLE IF NOT EXISTS drift_events (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        timestamp TEXT NOT NULL,
        resource_type TEXT NOT NULL,
        resource_id TEXT NOT NULL,
        expected TEXT,
        actual TEXT,
        source TEXT NOT NULL DEFAULT 'local',
        resolved_by INTEGER,
        FOREIGN KEY (resolved_by) REFERENCES applies(id)
    );

    CREATE TABLE IF NOT EXISTS managed_resources (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        resource_type TEXT NOT NULL,
        resource_id TEXT NOT NULL,
        source TEXT NOT NULL DEFAULT 'local',
        last_hash TEXT,
        last_applied INTEGER,
        UNIQUE(resource_type, resource_id),
        FOREIGN KEY (last_applied) REFERENCES applies(id)
    );

    CREATE TABLE IF NOT EXISTS config_sources (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        name TEXT NOT NULL UNIQUE,
        origin_url TEXT NOT NULL,
        origin_branch TEXT NOT NULL DEFAULT 'main',
        last_fetched TEXT,
        last_commit TEXT,
        source_version TEXT,
        pinned_version TEXT,
        status TEXT NOT NULL DEFAULT 'active'
    );

    CREATE TABLE IF NOT EXISTS source_applies (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        source_id INTEGER NOT NULL,
        apply_id INTEGER NOT NULL,
        source_commit TEXT NOT NULL,
        FOREIGN KEY (source_id) REFERENCES config_sources(id) ON DELETE CASCADE,
        FOREIGN KEY (apply_id) REFERENCES applies(id)
    );

    CREATE TABLE IF NOT EXISTS source_conflicts (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        timestamp TEXT NOT NULL,
        source_name TEXT NOT NULL,
        resource_type TEXT NOT NULL,
        resource_id TEXT NOT NULL,
        resolution TEXT NOT NULL,
        detail TEXT
    );

    CREATE TABLE IF NOT EXISTS pending_decisions (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        source      TEXT NOT NULL,
        resource    TEXT NOT NULL,
        tier        TEXT NOT NULL,
        action      TEXT NOT NULL,
        summary     TEXT NOT NULL,
        created_at  TEXT NOT NULL,
        resolved_at TEXT,
        resolution  TEXT
    );

    CREATE UNIQUE INDEX IF NOT EXISTS idx_pending_decisions_source_resource
        ON pending_decisions (source, resource)
        WHERE resolved_at IS NULL;

    CREATE TABLE IF NOT EXISTS source_config_hashes (
        source      TEXT PRIMARY KEY,
        config_hash TEXT NOT NULL,
        merged_at   TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS module_state (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        module_name     TEXT NOT NULL UNIQUE,
        installed_at    TEXT NOT NULL,
        last_applied    INTEGER,
        packages_hash   TEXT NOT NULL,
        files_hash      TEXT NOT NULL,
        git_sources     TEXT,
        status          TEXT NOT NULL DEFAULT 'installed',
        FOREIGN KEY (last_applied) REFERENCES applies(id)
    );

    CREATE TABLE IF NOT EXISTS schema_version (
        version INTEGER NOT NULL
    );

    INSERT INTO schema_version (version) VALUES (0);",
    // Migration 2: File safety — backup store, transaction journal, module file manifest
    "CREATE TABLE IF NOT EXISTS file_backups (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        apply_id        INTEGER NOT NULL,
        file_path       TEXT NOT NULL,
        content_hash    TEXT NOT NULL,
        content         BLOB NOT NULL,
        permissions     INTEGER,
        was_symlink     INTEGER NOT NULL DEFAULT 0,
        symlink_target  TEXT,
        oversized       INTEGER NOT NULL DEFAULT 0,
        backed_up_at    TEXT NOT NULL,
        FOREIGN KEY (apply_id) REFERENCES applies(id)
    );

    CREATE INDEX IF NOT EXISTS idx_file_backups_apply ON file_backups (apply_id);
    CREATE INDEX IF NOT EXISTS idx_file_backups_path ON file_backups (file_path);

    CREATE TABLE IF NOT EXISTS apply_journal (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        apply_id        INTEGER NOT NULL,
        action_index    INTEGER NOT NULL,
        phase           TEXT NOT NULL,
        action_type     TEXT NOT NULL,
        resource_id     TEXT NOT NULL,
        pre_state       TEXT,
        post_state      TEXT,
        status          TEXT NOT NULL DEFAULT 'pending',
        error           TEXT,
        started_at      TEXT NOT NULL,
        completed_at    TEXT,
        FOREIGN KEY (apply_id) REFERENCES applies(id)
    );

    CREATE INDEX IF NOT EXISTS idx_apply_journal_apply ON apply_journal (apply_id);

    CREATE TABLE IF NOT EXISTS module_file_manifest (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        module_name     TEXT NOT NULL,
        file_path       TEXT NOT NULL,
        content_hash    TEXT NOT NULL,
        strategy        TEXT NOT NULL,
        last_applied    INTEGER,
        UNIQUE(module_name, file_path),
        FOREIGN KEY (last_applied) REFERENCES applies(id)
    );

    CREATE INDEX IF NOT EXISTS idx_module_file_manifest_module ON module_file_manifest (module_name);",
    // Migration 3: Script output capture — store stdout/stderr from script actions
    "ALTER TABLE apply_journal ADD COLUMN script_output TEXT;",
    // Migration 4: Compliance snapshots — periodic machine state snapshots
    "CREATE TABLE IF NOT EXISTS compliance_snapshots (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        timestamp TEXT NOT NULL,
        content_hash TEXT NOT NULL,
        snapshot_json TEXT NOT NULL,
        summary_compliant INTEGER NOT NULL,
        summary_warning INTEGER NOT NULL,
        summary_violation INTEGER NOT NULL
    );",
    // Migration 5: persist the scripted uninstall command alongside each package
    // tracking row, so a custom/scripted manager's packages can still be pruned
    // after its definition leaves the config (the script vanishes with it).
    "ALTER TABLE managed_resources ADD COLUMN uninstall_cmd TEXT;",
    // Migration 6: source_applies.source_id gains ON DELETE CASCADE. An apply
    // records a source_applies row referencing config_sources(id); the bare
    // DELETE on config_sources then violated the foreign key (foreign_keys=ON),
    // so `source remove`/`source replace` failed after any apply — and the
    // cfgd.yaml mutation had already landed, leaving config and state out of
    // sync. SQLite cannot ALTER a constraint, so rebuild the child table (no
    // other table references source_applies, so the drop/rename is safe inside
    // the migration transaction; copied rows keep valid source_id references).
    "CREATE TABLE source_applies_new (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        source_id INTEGER NOT NULL,
        apply_id INTEGER NOT NULL,
        source_commit TEXT NOT NULL,
        FOREIGN KEY (source_id) REFERENCES config_sources(id) ON DELETE CASCADE,
        FOREIGN KEY (apply_id) REFERENCES applies(id)
    );
    INSERT INTO source_applies_new (id, source_id, apply_id, source_commit)
        SELECT id, source_id, apply_id, source_commit FROM source_applies;
    DROP TABLE source_applies;
    ALTER TABLE source_applies_new RENAME TO source_applies;",
    // Migration 7: daemon-snapshot drift resolution. `resolved_by` is a foreign
    // key into applies(id), so it cannot mark a row resolved when no apply ran
    // (the no-drift / healed-complement reconcile snapshots). A nullable
    // timestamp column — mirroring pending_decisions.resolved_at — carries that
    // marker without a synthetic apply row. "resolved" is now
    // `resolved_by IS NOT NULL OR resolved_at IS NOT NULL`.
    "ALTER TABLE drift_events ADD COLUMN resolved_at TEXT;",
    // Migration 8: durably record whether a file existed at backup time. A
    // pre-action backup of a CREATE action records existed=0 (an absent
    // marker); rollback then removes such files instead of restoring stale
    // content. DEFAULT 1 keeps every legacy row at today's content-restore
    // behavior.
    "ALTER TABLE file_backups ADD COLUMN existed INTEGER NOT NULL DEFAULT 1;",
    // Migration 9: drop managed_resources bookkeeping rows whose resource_id
    // shape changed. Four id derivations were corrected at once:
    //   - `Running script: {body}` and `system:{cfg}.{key} ({cur} → {des})`
    //     were split by a blind `splitn(3, ':')`, which truncated any body or
    //     value holding its own colon (URLs, `sed`/`awk` programs, PATH values).
    //   - `module:{name}:{verb}` dropped the module NAME, and
    //     `package:{manager}:{verb}` the MANAGER, so every module — and every
    //     manager's bootstrap/skip — collapsed onto one
    //     UNIQUE(resource_type, resource_id) row.
    //   - `secret:{decrypt,resolve}:…` keys were built with the native path
    //     separator, so a Windows-written key never matched its POSIX form.
    // Nothing sweeps managed_resources on observation (the only DELETE is
    // package-scoped), so a row written under an old id would linger in
    // `cfgd status` forever. These rows are pure bookkeeping — the next apply
    // re-derives them — and carry no uninstall_cmd, which only
    // `upsert_package_resource` ever writes, so deleting them loses nothing.
    // The package clause is scoped to the two collapsed ids rather than the
    // type: real package rows are keyed `{manager}/{package}` and DO carry an
    // uninstall_cmd that cannot be re-derived once its manager leaves config.
    "DELETE FROM managed_resources
        WHERE resource_type IN ('Running script', 'system', 'module', 'secret')
           OR (resource_type = 'package' AND resource_id IN ('bootstrap', 'skip'));",
    // Migration 10: fold the persisted file-path keys to `/`. Every writer of
    // `file_backups.file_path` and `module_file_manifest.file_path` now uses
    // `to_posix_fs_key`, so a Windows row written with the native separator
    // would no longer join: the manifest drives `latest_backup_for_path`, and a
    // mismatch there makes module removal DELETE a file it should have
    // RESTORED. These rows are normalized rather than dropped — unlike the
    // managed_resources bookkeeping above, `file_backups` holds the only copy
    // of pre-overwrite content and the manifest is the only record of what a
    // module deployed, so a DELETE would forfeit rollback.
    // `UPDATE OR REPLACE` because the manifest's UNIQUE(module_name, file_path)
    // is reachable, if barely: a module that declared both `C:\dir\f` and
    // `C:/dir/f` folds two historical rows onto one key. REPLACE keeps the row
    // being folded and drops the twin, which is safe because both name the same
    // file and the only column production reads from this table is file_path.
    // Scoped to Windows-rooted shapes on purpose. A backslash is a legal
    // filename character on unix, so folding `/home/u/od\d.conf` would re-point
    // the row at a different file — which is also why the writers fold on
    // Windows only. These columns hold absolute paths, so a unix row can never
    // begin `X:/`, `X:\`, or `\\`, and the three patterns between them catch
    // every Windows row including one authored with mixed separators.
    // SQLite gives LIKE no escape character, so `\` here is a literal.
    r"UPDATE file_backups
         SET file_path = REPLACE(file_path, '\', '/')
       WHERE file_path LIKE '_:\%' OR file_path LIKE '_:/%' OR file_path LIKE '\\%';

      UPDATE OR REPLACE module_file_manifest
         SET file_path = REPLACE(file_path, '\', '/')
       WHERE file_path LIKE '_:\%' OR file_path LIKE '_:/%' OR file_path LIKE '\\%';",
    // Migration 11: remember which package managers cfgd itself bootstrapped and
    // the PATH directories each contributed. `PackageManager::path_dirs()` is a
    // live probe of the machine — npm's creates a global prefix directory, and
    // brew's answer flips with what already exists on disk — so it is accurate
    // only in the instant after a successful bootstrap, and calling it from a
    // read-only path would mutate the user's home. The recorded answer is what
    // planning and verification read instead. `path_dirs` holds a JSON array
    // because the order is load-bearing: the generated shell file is hashed and
    // compared on every reconcile tick.
    "CREATE TABLE IF NOT EXISTS bootstrapped_managers (
        manager         TEXT PRIMARY KEY,
        path_dirs       TEXT NOT NULL,
        bootstrapped_at TEXT NOT NULL
    );",
    // Migration 12: persist the resolved global-install prefix a package
    // manager is actually using. `bootstrapped_managers` only gets a row when
    // cfgd itself installs the manager; most machines already have npm
    // present, so nothing ever writes one for it, and every install/uninstall
    // /update/list call re-derives a prefix from live-fallible inputs
    // (elevation, a write-probe, the project-local `.npmrc` npm itself
    // consults). A later run can legitimately re-derive a DIFFERENT prefix
    // than the one packages were actually installed under, making them
    // invisible to `installed_packages()` — the prefix must be decided once
    // and reused by every subsequent operation, not re-negotiated on each one.
    "CREATE TABLE IF NOT EXISTS package_manager_prefixes (
        manager     TEXT PRIMARY KEY,
        prefix      TEXT NOT NULL,
        is_fallback INTEGER NOT NULL,
        resolved_at TEXT NOT NULL
    );",
    // Migration 13: declarative backup runs (`spec.backups[]`). Distinct from
    // `file_backups`: that table stores pre-overwrite content inline for
    // rollback, while a backup run is an out-of-band snapshot whose payload
    // lives on the filesystem. `destination_path`/`size_bytes` are NULL when a
    // run produced no artifact (a pre-hook or copy failure), which is also the
    // predicate retention pruning uses to find deletable snapshots. Appended
    // after the migrations that shipped on master before it: the runner is
    // positional, so an element inserted mid-array is silently skipped by any
    // database already past that index.
    "CREATE TABLE IF NOT EXISTS backup_runs (
        id                INTEGER PRIMARY KEY AUTOINCREMENT,
        name              TEXT NOT NULL,
        source            TEXT NOT NULL,
        destination_path  TEXT,
        size_bytes        INTEGER,
        status            TEXT NOT NULL,
        error             TEXT,
        started_at        TEXT NOT NULL,
        finished_at       TEXT NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_backup_runs_name ON backup_runs (name);",
    // Migration 14: undouble the configurator name in persisted `system` ids.
    // Six configurators prefixed their OWN name into `SystemDrift::key` while
    // the reconciler composes `system:<configurator>.<key>` around it, so every
    // id they wrote carried the name twice (`sshKeys.sshKeys.default.exists`).
    // The id is what drift resolution, `resolve_drift_not_in` and the status
    // view match on, so leaving the old rows behind would strand each one as a
    // permanently-unresolved drift event beside its corrected twin.
    // Rewritten rather than deleted: unlike migration 9's shape change, the row
    // still names a resource cfgd manages, and `drift_events` carries the
    // observation history that a DELETE would forfeit.
    // The SET drops the first dot-segment — exactly the duplicated name —
    // instead of REPLACE, which would also rewrite a repeat later in the key.
    // GLOB, not LIKE: LIKE is ASCII-case-insensitive in SQLite, and these
    // prefixes are the configurators' exact-case names.
    // `UPDATE OR REPLACE` on managed_resources guards UNIQUE(resource_type,
    // resource_id), which the ordering argues is unreachable: `StateStore::open`
    // migrates before any write, so a corrected row cannot be written into a
    // store still below version 14, and the positional gate never replays this
    // once it is past. It stays because plain UPDATE would abort — and roll back
    // — the whole migration on a store whose schema_version was hand-edited or
    // restored from a partial backup, bricking every later open. The row REPLACE
    // drops is the corrected twin of the one being rewritten; both name the same
    // resource and the next apply re-derives it.
    // `compliance_snapshots` is included because `compliance diff` matches two
    // snapshots on each check's `key`, so an un-rewritten snapshot would report
    // every affected check as removed-and-re-added across the fix. Only the
    // identifier moves; status/value/detail — the observation itself — are
    // untouched. The anchor is `"key":"` with no spaces because
    // `store_compliance_snapshot` serializes compactly, and serde escapes an
    // embedded quote as `\"`, so the literal can only match a real member name;
    // `ComplianceCheck.key` is the only member named `key` in the snapshot.
    // This arm alone is not scoped by category, unlike the three `system` id
    // rewrites: the substitution is per-substring rather than per-check, so a
    // category predicate would not narrow it. Nothing else builds a check `key`
    // as `<configurator>.<drift key>`, which is what makes the six anchors safe.
    // The final statement re-derives `content_hash` through the write path's own
    // `snapshot_json_content_hash`, so a migrated row carries exactly the digest
    // a fresh write of the same content would produce. Every row is rehashed,
    // not just the rewritten ones: rows written before that derivation was
    // unified hashed a different serialization — and hashed the collection
    // timestamp, which the digest now excludes — so they hold a value the write
    // path can never mint again, and the sole consumer
    // (`latest_compliance_hash`, a change detector) reads it against a
    // freshly-derived one.
    r#"UPDATE OR REPLACE managed_resources
          SET resource_id = substr(resource_id, instr(resource_id, '.') + 1)
        WHERE resource_type = 'system'
          AND (resource_id GLOB 'sshKeys.sshKeys.*'
            OR resource_id GLOB 'gpgKeys.gpgKeys.*'
            OR resource_id GLOB 'seccomp.seccomp.*'
            OR resource_id GLOB 'apparmor.apparmor.*'
            OR resource_id GLOB 'containerd.containerd.*'
            OR resource_id GLOB 'kubelet.kubelet.*');

      UPDATE drift_events
          SET resource_id = substr(resource_id, instr(resource_id, '.') + 1)
        WHERE resource_type = 'system'
          AND (resource_id GLOB 'sshKeys.sshKeys.*'
            OR resource_id GLOB 'gpgKeys.gpgKeys.*'
            OR resource_id GLOB 'seccomp.seccomp.*'
            OR resource_id GLOB 'apparmor.apparmor.*'
            OR resource_id GLOB 'containerd.containerd.*'
            OR resource_id GLOB 'kubelet.kubelet.*');

      UPDATE apply_journal
          SET resource_id = substr(resource_id, instr(resource_id, '.') + 1)
        WHERE action_type = 'system'
          AND (resource_id GLOB 'sshKeys.sshKeys.*'
            OR resource_id GLOB 'gpgKeys.gpgKeys.*'
            OR resource_id GLOB 'seccomp.seccomp.*'
            OR resource_id GLOB 'apparmor.apparmor.*'
            OR resource_id GLOB 'containerd.containerd.*'
            OR resource_id GLOB 'kubelet.kubelet.*');

      UPDATE compliance_snapshots
          SET snapshot_json = REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
                snapshot_json,
                '"key":"sshKeys.sshKeys.',       '"key":"sshKeys.'),
                '"key":"gpgKeys.gpgKeys.',       '"key":"gpgKeys.'),
                '"key":"seccomp.seccomp.',       '"key":"seccomp.'),
                '"key":"apparmor.apparmor.',     '"key":"apparmor.'),
                '"key":"containerd.containerd.', '"key":"containerd.'),
                '"key":"kubelet.kubelet.',       '"key":"kubelet.')
        WHERE snapshot_json GLOB '*"key":"sshKeys.sshKeys.*'
           OR snapshot_json GLOB '*"key":"gpgKeys.gpgKeys.*'
           OR snapshot_json GLOB '*"key":"seccomp.seccomp.*'
           OR snapshot_json GLOB '*"key":"apparmor.apparmor.*'
           OR snapshot_json GLOB '*"key":"containerd.containerd.*'
           OR snapshot_json GLOB '*"key":"kubelet.kubelet.*';

      UPDATE compliance_snapshots
          SET content_hash = cfgd_compliance_content_hash(snapshot_json, content_hash);"#,
    // `action_index` is where an action sits in the run's plan; once package
    // work runs in per-manager lanes that stops being the order the actions
    // finished in, and the order they finished in stops being recoverable from
    // the schema at all. The backfill is exact rather than a guess: every
    // historical apply was sequential, so completion order WAS plan order.
    // A reporting and forensics column — the restore reads `file_backups`,
    // never the journal.
    "ALTER TABLE apply_journal ADD COLUMN completion_index INTEGER;
     UPDATE apply_journal SET completion_index = action_index;",
    // Migration 15: a single-row record of the last time this machine was
    // actually SCANNED for drift (a live `diff`/`verify`/`status --scan` pass,
    // or a daemon reconcile tick) — distinct from `drift_events`, which holds
    // rows only while something is actively drifting and goes empty on a
    // clean host. Without this a clean host has no signal at all for whether
    // its recorded-state `status` dashboard reflects a check from five
    // seconds ago or five weeks ago. `id = 1` pins it to one row: there is
    // exactly one "last scan" per machine, and an UPSERT keyed on that fixed
    // id is simpler than a MAX(timestamp) query over a growing table.
    "CREATE TABLE IF NOT EXISTS last_scan (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        timestamp TEXT NOT NULL
    );",
    // Migration 16: a column no longer read. It once told a restore's safety
    // copy apart from a run of the unit, back when the copy was stored as one
    // of the unit's snapshots; the copy now lives beside the source as a
    // `.cfgd-backup` sidecar and writes no row at all. A legacy `safety` row
    // still names a payload inside the unit's destination, and reads as the
    // ordinary snapshot it physically is. Migrations are append-only, so the
    // column stays.
    "ALTER TABLE backup_runs ADD COLUMN kind TEXT NOT NULL DEFAULT 'run';",
    // Migration 17: what the source declared for the item this row asks
    // about. Without it the only change signal was a hash over the source's
    // whole delivered set, which re-asked every answered item whenever an
    // unrelated item joined the set, and never re-asked an item whose own
    // declared value changed while the set stood still. NULL is "not
    // fingerprinted yet" and is deliberately not backfilled: the classifier
    // stamps the current fingerprint onto such a row without asking, so an
    // answer given before this column existed survives the upgrade.
    "ALTER TABLE pending_decisions ADD COLUMN content_hash TEXT;",
    // Migration 18: whether the commit a source was last fetched at carried a
    // signature cfgd would accept. Verification already asked the question on
    // every load, but only ever as a gate — nothing recorded the answer, so
    // `source list` could not tell an operator which of their sources are
    // signed without re-running git against every checkout. NULL is "not
    // known" (never fetched since this column existed, or a checkout cfgd
    // could not read) and is deliberately not backfilled to 0: an unsigned
    // source and an unreadable one are different facts.
    "ALTER TABLE config_sources ADD COLUMN last_commit_signed INTEGER;",
    // Migration 19: the per-file breakdown behind `last_hash` for a link-deployed
    // row, one `<path>:<sha256>` per line. `last_hash` is one aggregate over
    // every file a row stands for, so it can say THAT something moved but never
    // HOW MUCH: a one-line edit under a 52-file module tree read as 52 files
    // refreshed. NULL is "no breakdown recorded yet" and is backfilled silently
    // by the first refresh that sees the row; a refresh that finds no prior
    // breakdown reports no count rather than the row's coverage.
    "ALTER TABLE managed_resources ADD COLUMN file_hashes TEXT;",
    // Migration 20, two halves of one drift-identity settlement. The index:
    // `record_drift` is an UPDATE-then-SELECT per recorded row over a table
    // that only grows, and it, the per-key resolvers and the set-based
    // resolvers' row-value `IN` all seek this index; only the complement
    // (`NOT IN`) resolver still examines every unresolved row, which is
    // inherent to a complement. The retype: the daemon once recorded a failed
    // provision cascade as `('manager', 'provision:<name>')` and its planned
    // refusals as `('manager', 'refuse:<name>')`, while the CLI's live check
    // mints both under `package` — two spellings of one finding, and nothing
    // on a CLI-only host resolves the `manager` one. A legacy row with a
    // standing `package` twin is the duplicate and resolves; one without a
    // twin is the only record of the finding and is retyped so the live check
    // and the apply can settle it. Resolved rows keep their recorded type —
    // history describes what was written.
    "CREATE INDEX IF NOT EXISTS idx_drift_events_resource
         ON drift_events (resource_type, resource_id);

     UPDATE drift_events
         SET resolved_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')
       WHERE resource_type = 'manager'
         AND (resource_id GLOB 'provision:*' OR resource_id GLOB 'refuse:*')
         AND resolved_by IS NULL AND resolved_at IS NULL
         AND EXISTS (SELECT 1 FROM drift_events p
                      WHERE p.resource_type = 'package'
                        AND p.resource_id = drift_events.resource_id
                        AND p.resolved_by IS NULL AND p.resolved_at IS NULL);

     UPDATE drift_events
         SET resource_type = 'package'
       WHERE resource_type = 'manager'
         AND (resource_id GLOB 'provision:*' OR resource_id GLOB 'refuse:*')
         AND resolved_by IS NULL AND resolved_at IS NULL;",
];

/// Make `cfgd_compliance_content_hash(snapshot_json, current_hash)` callable
/// from migration SQL.
///
/// A migration that rewrites a stored snapshot has to re-derive the digest
/// stored beside it, and that digest is not a hash of the stored bytes: it
/// ignores the collection timestamp those bytes carry, so that an unchanged
/// machine hashes equal across collections. SQLite can express neither the
/// SHA-256 nor the normalization, so the write path's own derivation is
/// registered as a scalar function and the migration stays a plain SQL
/// statement — the alternative is a typed side-channel in the runner, which
/// would make migration order depend on Rust code rather than on the array.
///
/// `current_hash` is the passthrough for a `snapshot_json` that will not parse:
/// a corrupt row keeps the digest it had rather than failing the function,
/// which inside the runner's EXCLUSIVE transaction would roll back the whole
/// migration and leave the store unopenable for good.
///
/// Deterministic and innocuous: same input, same output, no side effects, so
/// SQLite may cache and reorder calls freely.
fn register_sql_functions(conn: &Connection) -> Result<()> {
    use rusqlite::functions::FunctionFlags;
    conn.create_scalar_function(
        "cfgd_compliance_content_hash",
        2,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            let snapshot_json = ctx.get_raw(0).as_str()?;
            let current_hash = ctx.get_raw(1).as_str()?;
            Ok(crate::compliance::snapshot_json_content_hash(snapshot_json)
                .unwrap_or_else(|_| current_hash.to_owned()))
        },
    )?;
    Ok(())
}

/// SQLite-backed state store for cfgd.
pub struct StateStore {
    pub(in crate::state) conn: Connection,
    /// Set for the duration of an [`StateStore::in_transaction`] call, so a
    /// nested call is caught at the call site that broke the rule instead of
    /// surfacing as a generic `BEGIN`-inside-`BEGIN` database error.
    in_transaction: Cell<bool>,
}

/// Rolls back an open [`StateStore::in_transaction`] batch unless it committed.
///
/// The `?` in `in_transaction` is what makes this a guard rather than a match
/// arm: an early return from `f` must not leave the connection inside a
/// transaction, because every later write on this store would then join a batch
/// nobody will commit.
struct TransactionGuard<'a> {
    conn: &'a Connection,
    finished: bool,
}

impl Drop for TransactionGuard<'_> {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.conn.execute_batch("ROLLBACK");
        }
    }
}

/// Clears [`StateStore::in_transaction`]'s nesting flag on drop, so an early
/// `?` return or a panic inside `f` still leaves the next, sequential call
/// free to proceed rather than finding the flag stuck `true` forever.
struct NestingGuard<'a> {
    flag: &'a Cell<bool>,
}

impl Drop for NestingGuard<'_> {
    fn drop(&mut self) {
        self.flag.set(false);
    }
}

impl StateStore {
    /// Open or create a state store at the default location.
    /// Uses `~/.local/state/cfgd/state.db`.
    pub fn open_default() -> Result<Self> {
        Self::open_in_dir(&default_state_dir()?)
    }

    /// The database file this store is connected to, or `None` when it is not
    /// backed by one.
    ///
    /// sqlite answers an EMPTY path for a temporary or in-memory database, which
    /// is normalized to `None` here so the two "no file" answers are one and a
    /// caller never asks the filesystem about `""`.
    ///
    /// For a holder that keeps a connection open across units of work: cfgd
    /// itself relocates the database (the legacy-state-dir migration inside
    /// [`Self::open`]), and a connection survives that relocation attached to an
    /// inode the path no longer names. Comparing this path's
    /// [`crate::file_identity`] against the one captured at open is how such a
    /// holder notices, since neither sqlite nor the filesystem reports it.
    pub fn db_path(&self) -> Option<&Path> {
        self.conn
            .path()
            .filter(|path| !path.is_empty())
            .map(Path::new)
    }

    /// Open or create the state store at `scope`'s default location — the
    /// fallback for a daemon path whose materialized state dir is absent:
    /// re-deriving from scope either lands on the same directory the loop
    /// would have carried or fails the same way the materialization did,
    /// where an unqualified [`Self::open_default`] would silently hand a
    /// system-scope daemon the per-user store.
    pub fn open_default_for(scope: crate::Scope) -> Result<Self> {
        Self::open_in_dir(&default_state_dir_for(scope)?)
    }

    /// Open or create the canonical [`STATE_DB_FILENAME`] DB inside `dir`,
    /// creating the directory if needed. Every state-dir override resolves the
    /// DB through here so it cannot drift from the default-location filename.
    pub fn open_in_dir(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).map_err(|_| StateError::DirectoryNotWritable {
            path: dir.to_path_buf(),
        })?;
        // create_dir_all is a no-op when the dir already exists, so an existing
        // read-only state dir would otherwise reach Connection::open and fail with
        // a generic "state database error" that names no path. Probe real write
        // access first so a read-only dir surfaces the typed DirectoryNotWritable.
        if let crate::DirWritable::NotWritable = crate::probe_dir_writable(dir) {
            return Err(StateError::DirectoryNotWritable {
                path: dir.to_path_buf(),
            }
            .into());
        }
        Self::open(&dir.join(STATE_DB_FILENAME))
    }

    /// Open or create a state store at the given path.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        // `synchronous=NORMAL` is the WAL-mode counterpart of the default
        // `FULL`: a committing writer stops fsyncing the WAL on every commit and
        // syncs at checkpoints instead. It is safe precisely BECAUSE `WAL` is
        // set on the line beside it — under WAL, `NORMAL` still cannot lose or
        // corrupt a committed transaction when the PROCESS dies (the WAL is
        // durable in the page cache and replayed on the next open); the window
        // it trades away is an OS crash or power loss between commit and
        // checkpoint, which costs the most recent applies' journal rows on a
        // machine that just lost power mid-apply. An apply writes one row per
        // action plus a backup blob per touched file, and paying a disk sync for
        // each was the single largest fixed cost of a large apply.
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;",
        )?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        register_sql_functions(&conn)?;

        let mut store = Self {
            conn,
            in_transaction: Cell::new(false),
        };
        store.run_migrations()?;
        Ok(store)
    }

    /// Run `f` with every write it performs committed as ONE transaction.
    ///
    /// Every `StateStore` write method takes `&self` and so runs in SQLite's
    /// implicit per-statement transaction: a run recording a hundred managed
    /// resources took a hundred commits, each one a WAL write and (before
    /// `synchronous=NORMAL`) a disk sync. Wrap a LOOP of writes in this and the
    /// whole loop costs one.
    ///
    /// `f` returning `Err` rolls the batch back, so a partially-recorded loop
    /// never survives the failure that interrupted it — the caller's `?`
    /// already abandons the run at that point, and half a bookkeeping sweep is
    /// worse to reason about than none. A panic inside `f` rolls back too, via
    /// the guard's `Drop`.
    ///
    /// NOT for a write whose whole value is being on disk BEFORE the next thing
    /// happens: the journal's per-action begin/finish rows and the pre-action
    /// file backups are the record a crashed apply is reconstructed from, and
    /// batching them would lose exactly the rows describing the action that
    /// crashed. Transactions do not nest — never call this from inside `f`.
    /// A debug build catches the violation here, at the call site that broke
    /// the rule, with a `debug_assert` naming it; release behavior is
    /// unchanged and still fails with the generic `BEGIN`-inside-`BEGIN`
    /// database error the inner `BEGIN` reports.
    pub fn in_transaction<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        debug_assert!(
            !self.in_transaction.get(),
            "StateStore::in_transaction does not nest — never call this from inside `f`"
        );
        self.in_transaction.set(true);
        let _nesting_guard = NestingGuard {
            flag: &self.in_transaction,
        };

        self.conn.execute_batch("BEGIN")?;
        let mut guard = TransactionGuard {
            conn: &self.conn,
            finished: false,
        };
        let value = f()?;
        self.conn.execute_batch("COMMIT")?;
        guard.finished = true;
        Ok(value)
    }

    /// Create an in-memory state store (for testing).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        register_sql_functions(&conn)?;

        let mut store = Self {
            conn,
            in_transaction: Cell::new(false),
        };
        store.run_migrations()?;
        Ok(store)
    }

    /// Remove the `backup_runs` table so the next write to it fails.
    ///
    /// The seam for a caller's state-store-failure arm, which in production is
    /// reached only by a refused write (a full disk, a locked or corrupt DB)
    /// and is otherwise untestable: the connection is private to this module,
    /// so a consumer's test cannot break the schema by hand.
    #[cfg(test)]
    pub(crate) fn drop_backup_runs_table(&self) -> Result<()> {
        self.conn.execute("DROP TABLE backup_runs", [])?;
        Ok(())
    }

    fn run_migrations(&mut self) -> Result<()> {
        // Use EXCLUSIVE transaction to serialize concurrent migration attempts
        // (e.g. parallel cargo test processes sharing the same state DB).
        self.conn
            .execute_batch("BEGIN EXCLUSIVE")
            .map_err(|e| StateError::MigrationFailed {
                message: format!("failed to acquire migration lock: {e}"),
            })?;

        let current_version = self.schema_version().inspect_err(|_| {
            if let Err(rb) = self.conn.execute_batch("ROLLBACK") {
                tracing::error!("rollback after schema_version read failure also failed: {rb}");
            }
        })?;

        for (i, migration) in MIGRATIONS.iter().enumerate() {
            if i >= current_version {
                self.conn.execute_batch(migration).map_err(|e| {
                    if let Err(rb) = self.conn.execute_batch("ROLLBACK") {
                        tracing::error!("rollback after migration {i} failure also failed: {rb}");
                    }
                    StateError::MigrationFailed {
                        message: format!("migration {}: {}", i, e),
                    }
                })?;
                // Set version automatically — no hardcoded UPDATE in migration SQL
                let new_version = (i + 1) as i64;
                self.conn
                    .execute(
                        "UPDATE schema_version SET version = ?1",
                        rusqlite::params![new_version],
                    )
                    .map_err(|e| {
                        if let Err(rb) = self.conn.execute_batch("ROLLBACK") {
                            tracing::error!(
                                "rollback after schema_version update failure also failed: {rb}"
                            );
                        }
                        StateError::MigrationFailed {
                            message: format!("migration {}: failed to update version: {}", i, e),
                        }
                    })?;
            }
        }

        self.conn
            .execute_batch("COMMIT")
            .map_err(|e| StateError::MigrationFailed {
                message: format!("failed to commit migrations: {e}"),
            })?;

        Ok(())
    }

    /// The applied-migration count, or `0` for a database that has never run
    /// one. A read failure (locked, corrupt, mid-crash file) must propagate
    /// rather than fold to `0`: several migrations are `ALTER TABLE ... ADD
    /// COLUMN`, which is not idempotent, so a spurious `0` here makes
    /// [`Self::run_migrations`] replay them against a database that already
    /// has the column and abort with "duplicate column name" — turning a
    /// transient read error into a permanent open failure. The only
    /// legitimate `0` is "the `schema_version` table itself does not exist
    /// yet", checked directly rather than inferred from whatever error a
    /// missing table happens to raise.
    fn schema_version(&self) -> Result<usize> {
        let table_exists: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'schema_version'",
            [],
            |row| row.get(0),
        )?;
        if table_exists == 0 {
            return Ok(0);
        }

        let version: i64 =
            self.conn
                .query_row("SELECT version FROM schema_version", [], |row| row.get(0))?;
        Ok(version as usize)
    }
}

/// Compute SHA256 hash of a serializable plan for deduplication.
pub fn plan_hash(data: &str) -> String {
    crate::sha256_hex(data.as_bytes())
}

/// Default per-user state directory (SQLite state DB, backups).
///
/// Resolution order:
/// 1. `CFGD_STATE_DIR` when set (verbatim) — back-compat short-circuit, wins
///    over everything.
/// 2. the platform-native state location with a `cfgd` segment, honoring
///    `XDG_STATE_HOME`:
///    - Linux: `$XDG_STATE_HOME/cfgd` (default `~/.local/state/cfgd`)
///    - macOS/Windows: `BaseDirs::state_dir()` is `None`, so fall back to the
///      data-local dir nested under a `state/` segment
///      (macOS `~/Library/Application Support/cfgd/state`,
///      Windows `%LOCALAPPDATA%\cfgd\state`).
///
/// Resolves home through the same policy config discovery uses (HOME on Unix,
/// USERPROFILE/HOME on Windows) so an unset HOME fails uniformly instead of
/// creating an orphan state.db beside a config error.
///
/// Honors the [`crate::TestHomeGuard`] thread-local override (test builds
/// resolve a Linux-shaped `~/.local/state/cfgd` under the override home) so
/// tests never write to the real state directory.
pub fn default_state_dir() -> Result<PathBuf> {
    default_state_dir_for(Scope::User)
}

/// Scope-aware state directory.
///
/// Precedence (highest first): `CFGD_STATE_DIR` (verbatim), systemd's
/// `$STATE_DIRECTORY`, then the scope default. [`Scope::User`] is the frozen
/// resolution documented on [`default_state_dir`]. [`Scope::System`] is the
/// absolute machine-wide state root (Linux `/var/lib/cfgd`, macOS
/// `/Library/Application Support/cfgd/state`, Windows `%ProgramData%\cfgd\state`)
/// and consults no home directory, so it never errors. Pure path logic.
pub fn default_state_dir_for(scope: Scope) -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("CFGD_STATE_DIR") {
        return Ok(PathBuf::from(dir));
    }
    if let Some(dir) = crate::systemd_dir("STATE_DIRECTORY") {
        return Ok(dir);
    }
    if scope.is_system() {
        return Ok(system_state_dir());
    }
    if let Some(home) = crate::test_home_override() {
        return Ok(home.join(".local").join("state").join("cfgd"));
    }
    // `directories` would otherwise fall back to the passwd database when HOME
    // is unset, resolving a home that config discovery cannot — the two
    // subsystems must agree.
    if crate::home_dir_var().is_none() {
        return Err(StateError::DirectoryNotWritable {
            path: PathBuf::from("~/.local/state/cfgd"),
        }
        .into());
    }
    let base = directories::BaseDirs::new().ok_or_else(|| StateError::DirectoryNotWritable {
        path: PathBuf::from("~/.local/state/cfgd"),
    })?;
    Ok(match base.state_dir() {
        Some(state) => state.join("cfgd"),
        None => base.data_local_dir().join("cfgd").join("state"),
    })
}

/// The machine-wide state root: Linux `/var/lib/cfgd`, macOS
/// `/Library/Application Support/cfgd/state`, Windows `%ProgramData%\cfgd\state`.
/// Absolute on every platform — never consults a home directory.
fn system_state_dir() -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        PathBuf::from("/var/lib/cfgd")
    }
    #[cfg(target_os = "macos")]
    {
        PathBuf::from("/Library/Application Support/cfgd/state")
    }
    #[cfg(windows)]
    {
        crate::program_data_dir().join("cfgd").join("state")
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        PathBuf::from("/var/lib/cfgd")
    }
}

/// Crash-safe move of the state DB (and any unfolded WAL/SHM sidecars) from a
/// legacy directory to a new one. Returns `true` when a DB was migrated,
/// `false` when there was nothing to do.
///
/// Never clobbers: when a DB already exists at `new_dir` the legacy DB is left
/// in place and `false` is returned, so a partial earlier migration (or a fresh
/// install that already wrote to the new location) is never overwritten.
///
/// Before moving, a best-effort `wal_checkpoint(TRUNCATE)` is run against a bare
/// connection (no schema migrations) to fold the WAL into the main DB file. When
/// the checkpoint succeeds the folded sidecars are removed; when it fails (a
/// locked or degraded DB) the sidecars — snapshotted before any connection opens,
/// since opening one would let SQLite delete them — are recreated beside the
/// moved DB so no committed-but-unfolded data is lost.
pub fn migrate_state_db(legacy_dir: &Path, new_dir: &Path) -> Result<bool> {
    let legacy_db = legacy_dir.join(STATE_DB_FILENAME);
    let new_db = new_dir.join(STATE_DB_FILENAME);
    if !legacy_db.exists() {
        return Ok(false);
    }
    // A DB already at the destination is authoritative — never overwrite it.
    if new_db.exists() {
        return Ok(false);
    }

    // Snapshot the sidecar bytes BEFORE opening any connection: opening (and
    // dropping) a connection to the DB makes SQLite delete what it judges to be
    // stale `-wal`/`-shm` files, which would destroy the recovery copies before
    // the checkpoint-failure branch below could carry them across. Read them up
    // front so a degraded/locked DB never loses committed-but-unfolded data.
    let sidecars: Vec<(&str, Vec<u8>)> = ["-wal", "-shm"]
        .into_iter()
        .filter_map(|suffix| {
            let path = legacy_dir.join(format!("{STATE_DB_FILENAME}{suffix}"));
            std::fs::read(&path).ok().map(|bytes| (suffix, bytes))
        })
        .collect();

    // Fold the WAL into the main DB so the moved file is self-contained. A bare
    // connection is used (not StateStore::open) so this never runs schema
    // migrations on an old DB mid-move. Failure (locked/degraded DB) is captured
    // and recovered from below by writing the snapshotted sidecars instead.
    let checkpointed = match Connection::open(&legacy_db) {
        Ok(conn) => conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .is_ok(),
        Err(_) => false,
    };

    std::fs::create_dir_all(new_dir).map_err(|e| StateError::FilesystemIo {
        path: new_dir.to_path_buf(),
        source: e,
    })?;
    crate::move_file(&legacy_db, &new_db).map_err(|e| StateError::FilesystemIo {
        path: new_db.clone(),
        source: e,
    })?;

    // When the checkpoint succeeded the WAL is folded and truncated, so the moved
    // DB is self-contained and the sidecars hold nothing extra — drop any that
    // survive at legacy. When it failed, recreate the snapshotted sidecars beside
    // the moved DB so committed-but-unfolded data is preserved.
    if checkpointed {
        for suffix in ["-wal", "-shm"] {
            let _ = std::fs::remove_file(legacy_dir.join(format!("{STATE_DB_FILENAME}{suffix}")));
        }
    } else {
        for (suffix, bytes) in &sidecars {
            let new_sidecar = new_dir.join(format!("{STATE_DB_FILENAME}{suffix}"));
            let _ = std::fs::write(&new_sidecar, bytes);
            let _ = std::fs::remove_file(legacy_dir.join(format!("{STATE_DB_FILENAME}{suffix}")));
        }
    }

    Ok(true)
}

#[cfg(test)]
mod tests;
