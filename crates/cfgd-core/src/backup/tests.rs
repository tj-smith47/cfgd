use std::path::{Path, PathBuf};

use super::*;
use crate::config::ScriptEntry;
use crate::output::Printer;

/// A backup spec with everything but `name`/`source` left at its default.
fn spec(name: &str, source: &Path) -> BackupSpec {
    serde_yaml::from_str(&format!(
        "name: {name}\nsource: {}\n",
        crate::to_posix_string(source)
    ))
    .expect("minimal backup spec should parse")
}

fn hook(run: &str) -> ScriptEntry {
    ScriptEntry::Simple(run.to_string())
}

/// An inline hook that creates `path` as an empty marker file, written for the
/// interpreter `ScriptShell::Auto` picks on this platform (`sh` on Unix,
/// `cmd.exe` on Windows) so the hook tests run everywhere rather than being
/// Unix-gated.
fn touch_hook(path: &Path) -> ScriptEntry {
    // native-ok: the path is interpolated into a shell command for THIS host.
    #[cfg(unix)]
    let run = format!("touch '{}'", path.display());
    #[cfg(windows)]
    let run = format!("type nul > \"{}\"", path.display());
    hook(&run)
}

/// An inline hook that writes the named environment variables, colon-joined,
/// into `path`. Read back with [`marker_contents`], which trims the trailing
/// newline `cmd.exe`'s `echo` appends.
fn echo_env_hook(vars: &[&str], path: &Path) -> ScriptEntry {
    // native-ok: the path is interpolated into a shell command for THIS host.
    #[cfg(unix)]
    let run = {
        let refs = vars
            .iter()
            .map(|v| format!("${v}"))
            .collect::<Vec<_>>()
            .join(":");
        format!("printf '%s' \"{refs}\" > '{}'", path.display())
    };
    #[cfg(windows)]
    let run = {
        let refs = vars
            .iter()
            .map(|v| format!("%{v}%"))
            .collect::<Vec<_>>()
            .join(":");
        format!("echo {refs}> \"{}\"", path.display())
    };
    hook(&run)
}

fn marker_contents(path: &Path) -> String {
    std::fs::read_to_string(path)
        .expect("marker file should exist")
        .trim()
        .to_string()
}

/// Layout shared by every engine test: an isolated home, a config dir, a state
/// dir, an in-memory store, and a capturing printer.
struct Harness {
    _home: tempfile::TempDir,
    root: PathBuf,
    store: StateStore,
    printer: Printer,
}

impl Harness {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("tempdir");
        let root = home.path().to_path_buf();
        std::fs::create_dir_all(root.join("config")).expect("config dir");
        std::fs::create_dir_all(root.join("state")).expect("state dir");
        let (printer, _) = Printer::for_test();
        Self {
            _home: home,
            root,
            store: StateStore::open_in_memory().expect("in-memory store"),
            printer,
        }
    }

    fn config_dir(&self) -> PathBuf {
        self.root.join("config")
    }

    fn state_dir(&self) -> PathBuf {
        self.root.join("state")
    }

    /// Run `spec` with the harness's real config/state dirs under an isolated
    /// `$HOME`, so hook working directories and `~` expansion never touch the
    /// developer's home.
    fn run(&self, spec: &BackupSpec) -> BackupRunRecord {
        let config_dir = self.config_dir();
        let state_dir = self.state_dir();
        crate::with_test_home(&self.root, || {
            let unit = BackupUnit::new(spec, &config_dir, "workstation", &state_dir);
            run_backup(&unit, &self.store, &self.printer).expect("run must be recorded")
        })
    }
}

fn snapshot_dir(h: &Harness, name: &str) -> PathBuf {
    h.state_dir().join("backups").join(name)
}

fn snapshots(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    names
}

// ---------------------------------------------------------------------------
// Unit binding
// ---------------------------------------------------------------------------

#[test]
fn destination_defaults_under_the_state_dir() {
    let s = spec("db", Path::new("/var/lib/app/data.db"));
    let unit = BackupUnit::new(&s, Path::new("/cfg"), "workstation", Path::new("/state"));
    assert_eq!(
        unit.destination_dir(),
        PathBuf::from("/state").join("backups").join("db")
    );
    assert_eq!(unit.source(), PathBuf::from("/var/lib/app/data.db"));
    assert_eq!(unit.spec().name, "db");
}

#[test]
fn explicit_destination_wins_over_the_default() {
    let mut s = spec("db", Path::new("/var/lib/app/data.db"));
    s.destination = Some(PathBuf::from("/srv/backups/app"));
    let unit = BackupUnit::new(&s, Path::new("/cfg"), "workstation", Path::new("/state"));
    assert_eq!(unit.destination_dir(), PathBuf::from("/srv/backups/app"));
}

#[test]
fn tilde_in_source_and_destination_expands_to_the_home_dir() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut s = spec("db", Path::new("~/data.db"));
    s.destination = Some(PathBuf::from("~/snapshots"));
    crate::with_test_home(home.path(), || {
        let unit = BackupUnit::new(&s, Path::new("/cfg"), "workstation", Path::new("/state"));
        assert_eq!(unit.source(), home.path().join("data.db"));
        assert_eq!(unit.destination_dir(), home.path().join("snapshots"));
    });
}

#[test]
fn context_selects_the_cfgd_context_hooks_observe() {
    let s = spec("db", Path::new("/nonexistent"));
    let base = BackupUnit::new(&s, Path::new("/cfg"), "workstation", Path::new("/state"));
    assert_eq!(base.context, ReconcileContext::Apply);
    assert_eq!(
        base.with_context(ReconcileContext::Reconcile).context,
        ReconcileContext::Reconcile
    );
}

// ---------------------------------------------------------------------------
// Copy: file sources
// ---------------------------------------------------------------------------

#[test]
fn file_source_is_snapshotted_and_recorded() {
    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, b"payload-bytes").expect("write source");

    let record = h.run(&spec("db", &source));

    assert_eq!(record.status, BackupRunStatus::Success);
    assert!(record.is_clean(), "clean run carried an error: {record:?}");
    assert_eq!(record.size_bytes, Some(13));
    assert_eq!(record.source, crate::to_posix_string(&source));

    let dest = record.destination_path.clone().expect("artifact recorded");
    let dest = PathBuf::from(dest);
    assert_eq!(
        std::fs::read(&dest).expect("snapshot readable"),
        b"payload-bytes"
    );
    assert_eq!(
        dest.parent(),
        Some(snapshot_dir(&h, "db").as_path()),
        "snapshot landed outside the default destination"
    );
    // Default namePattern is "{filename}.{timestamp}".
    let name = dest
        .file_name()
        .expect("name")
        .to_string_lossy()
        .to_string();
    assert!(
        name.starts_with("data.db."),
        "unexpected snapshot name: {name}"
    );
}

#[test]
fn name_pattern_variables_are_substituted() {
    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, b"x").expect("write source");
    let mut s = spec("nightly", &source);
    s.name_pattern = "{name}-{filename}-{timestamp}.snap".to_string();

    let record = h.run(&s);

    let name = PathBuf::from(record.destination_path.expect("artifact"))
        .file_name()
        .expect("name")
        .to_string_lossy()
        .to_string();
    assert!(name.starts_with("nightly-data.db-"), "got {name}");
    assert!(name.ends_with(".snap"), "got {name}");
    // The timestamp slot rendered a real stamp, not a literal token.
    assert!(!name.contains('{'), "unrendered token in {name}");
}

#[test]
fn explicit_destination_receives_the_snapshot() {
    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, b"x").expect("write source");
    let mut s = spec("db", &source);
    let dest = h.root.join("elsewhere").join("nested");
    s.destination = Some(dest.clone());

    let record = h.run(&s);

    let path = PathBuf::from(record.destination_path.expect("artifact"));
    assert_eq!(path.parent(), Some(dest.as_path()));
    assert!(path.exists());
}

#[test]
fn a_same_second_rerun_replaces_the_colliding_snapshot() {
    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, b"first").expect("write source");
    let mut s = spec("db", &source);
    // Drop {timestamp} so both runs render the identical name.
    s.name_pattern = "{filename}".to_string();

    let first = h.run(&s);
    std::fs::write(&source, b"second-and-longer").expect("rewrite source");
    let second = h.run(&s);

    assert_eq!(first.destination_path, second.destination_path);
    let path = PathBuf::from(second.destination_path.expect("artifact"));
    assert_eq!(
        std::fs::read(&path).expect("snapshot readable"),
        b"second-and-longer"
    );
    assert_eq!(second.size_bytes, Some(17));
}

// ---------------------------------------------------------------------------
// Copy: directory sources
// ---------------------------------------------------------------------------

#[test]
fn directory_source_is_copied_recursively() {
    let h = Harness::new();
    let source = h.root.join("tree");
    std::fs::create_dir_all(source.join("nested/deeper")).expect("tree");
    std::fs::write(source.join("top.txt"), b"aaa").expect("file");
    std::fs::write(source.join("nested/mid.txt"), b"bb").expect("file");
    std::fs::write(source.join("nested/deeper/leaf.txt"), b"c").expect("file");

    let record = h.run(&spec("tree", &source));

    assert_eq!(record.status, BackupRunStatus::Success);
    let dest = PathBuf::from(record.destination_path.expect("artifact"));
    assert!(dest.is_dir(), "directory snapshot is not a directory");
    assert_eq!(std::fs::read(dest.join("top.txt")).expect("top"), b"aaa");
    assert_eq!(
        std::fs::read(dest.join("nested/mid.txt")).expect("mid"),
        b"bb"
    );
    assert_eq!(
        std::fs::read(dest.join("nested/deeper/leaf.txt")).expect("leaf"),
        b"c"
    );
    assert_eq!(record.size_bytes, Some(6), "size must sum the whole tree");
}

#[test]
fn directory_snapshot_leaves_no_staging_directory_behind() {
    let h = Harness::new();
    let source = h.root.join("tree");
    std::fs::create_dir_all(&source).expect("tree");
    std::fs::write(source.join("a.txt"), b"a").expect("file");

    h.run(&spec("tree", &source));

    let leftovers: Vec<String> = snapshots(&snapshot_dir(&h, "tree"))
        .into_iter()
        .filter(|n| n.starts_with('.'))
        .collect();
    assert!(leftovers.is_empty(), "staging leftovers: {leftovers:?}");
}

#[cfg(unix)]
#[test]
fn directory_snapshot_skips_symlinks_out_of_the_source_tree() {
    let h = Harness::new();
    let outside = h.root.join("secret.txt");
    std::fs::write(&outside, b"do-not-copy").expect("outside file");
    let source = h.root.join("tree");
    std::fs::create_dir_all(&source).expect("tree");
    std::fs::write(source.join("real.txt"), b"ok").expect("file");
    std::os::unix::fs::symlink(&outside, source.join("link.txt")).expect("symlink");

    let record = h.run(&spec("tree", &source));

    let dest = PathBuf::from(record.destination_path.expect("artifact"));
    assert!(dest.join("real.txt").exists());
    assert!(
        !dest.join("link.txt").exists(),
        "symlink was followed into the snapshot"
    );
}

// ---------------------------------------------------------------------------
// Copy failures
// ---------------------------------------------------------------------------

#[test]
fn missing_source_records_a_failed_run_with_no_artifact() {
    let h = Harness::new();
    let record = h.run(&spec("db", &h.root.join("gone.db")));

    assert_eq!(record.status, BackupRunStatus::Failed);
    assert!(!record.has_artifact());
    assert!(!record.is_clean());
    assert_eq!(record.size_bytes, None);
    let error = record.error.clone().expect("failure detail");
    assert!(
        error.contains("source does not exist"),
        "unhelpful error: {error}"
    );
    assert!(error.contains("gone.db"), "error omits the path: {error}");
}

#[test]
fn a_failed_copy_still_writes_a_run_row() {
    let h = Harness::new();
    h.run(&spec("db", &h.root.join("gone.db")));

    let runs = h.store.backup_runs("db").expect("history");
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, BackupRunStatus::Failed);
    assert_eq!(runs[0].destination_path, None);
}

// ---------------------------------------------------------------------------
// Hooks
// ---------------------------------------------------------------------------

#[test]
fn pre_hook_failure_skips_the_snapshot_but_still_runs_post_hooks() {
    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, b"payload").expect("write source");
    let marker = h.root.join("post-ran");
    let mut s = spec("db", &source);
    s.pre_backup = vec![hook("exit 7")];
    s.post_backup = vec![touch_hook(&marker)];

    let record = h.run(&s);

    assert_eq!(record.status, BackupRunStatus::Failed);
    assert!(!record.has_artifact());
    let error = record.error.clone().expect("failure detail");
    assert!(error.contains("preBackup"), "phase missing from: {error}");
    // A half-run preBackup list is exactly when the machine is left stopped, so
    // postBackup — the thing that restarts it — must still get its chance.
    assert!(
        marker.exists(),
        "postBackup must run even when preBackup failed"
    );
    assert!(
        snapshots(&snapshot_dir(&h, "db")).is_empty(),
        "a skipped snapshot still wrote to the destination"
    );
}

#[test]
fn a_failed_pre_hook_and_a_failed_post_hook_are_both_recorded() {
    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, b"payload").expect("write source");
    let mut s = spec("db", &source);
    s.pre_backup = vec![hook("exit 7")];
    s.post_backup = vec![hook("exit 9")];

    let record = h.run(&s);

    assert_eq!(record.status, BackupRunStatus::Failed);
    let error = record.error.clone().expect("failure detail");
    assert!(error.contains("preBackup"), "pre failure lost: {error}");
    assert!(error.contains("postBackup"), "post failure lost: {error}");
}

#[test]
fn pre_hook_success_lets_the_snapshot_proceed() {
    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, b"payload").expect("write source");
    let marker = h.root.join("pre-ran");
    let mut s = spec("db", &source);
    s.pre_backup = vec![touch_hook(&marker)];

    let record = h.run(&s);

    assert!(marker.exists(), "preBackup did not run");
    assert!(record.is_clean(), "{record:?}");
}

#[test]
fn post_hook_runs_after_a_failed_copy() {
    let h = Harness::new();
    let marker = h.root.join("post-ran");
    let mut s = spec("db", &h.root.join("gone.db"));
    s.post_backup = vec![touch_hook(&marker)];

    let record = h.run(&s);

    assert!(
        marker.exists(),
        "postBackup must run even when the copy failed"
    );
    assert_eq!(record.status, BackupRunStatus::Failed);
}

#[test]
fn a_failed_copy_and_a_failed_post_hook_are_both_reported() {
    let h = Harness::new();
    let mut s = spec("db", &h.root.join("gone.db"));
    s.post_backup = vec![hook("exit 9")];

    let record = h.run(&s);

    assert_eq!(record.status, BackupRunStatus::Failed);
    let error = record.error.clone().expect("failure detail");
    assert!(
        error.contains("source does not exist"),
        "copy failure lost: {error}"
    );
    assert!(
        error.contains("postBackup"),
        "post-hook failure lost: {error}"
    );
}

#[test]
fn post_hook_failure_after_a_good_copy_keeps_the_run_successful() {
    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, b"payload").expect("write source");
    let mut s = spec("db", &source);
    s.post_backup = vec![hook("exit 3")];

    let record = h.run(&s);

    // The snapshot is complete and restorable, so the run stays Success and
    // retention-eligible; the hook failure surfaces through `error`.
    assert_eq!(record.status, BackupRunStatus::Success);
    assert!(record.has_artifact());
    assert!(
        !record.is_clean(),
        "a post-hook failure must not read as clean"
    );
    let error = record.error.clone().expect("failure detail");
    assert!(error.contains("postBackup"), "got: {error}");
    let dest = PathBuf::from(record.destination_path.expect("artifact"));
    assert_eq!(std::fs::read(&dest).expect("snapshot"), b"payload");
}

#[test]
fn hooks_see_the_backup_phase_in_the_environment() {
    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, b"payload").expect("write source");
    let pre = h.root.join("pre-phase");
    let post = h.root.join("post-phase");
    let mut s = spec("db", &source);
    s.pre_backup = vec![echo_env_hook(&["CFGD_PHASE"], &pre)];
    s.post_backup = vec![echo_env_hook(&["CFGD_PHASE", "CFGD_PROFILE"], &post)];

    h.run(&s);

    assert_eq!(marker_contents(&pre), "preBackup");
    assert_eq!(marker_contents(&post), "postBackup:workstation");
}

#[test]
fn a_continue_on_error_pre_hook_still_skips_the_snapshot() {
    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, b"payload").expect("write source");
    let marker = h.root.join("second-ran");
    let mut s = spec("db", &source);
    s.pre_backup = vec![
        serde_yaml::from_str("run: exit 5\ncontinueOnError: true\n").expect("hook"),
        touch_hook(&marker),
    ];

    let record = h.run(&s);

    // continueOnError governs the rest of the hook LIST, not the snapshot: the
    // second hook runs, but the recorded failure still skips the copy.
    assert!(marker.exists(), "continueOnError did not continue the list");
    assert_eq!(record.status, BackupRunStatus::Failed);
    assert!(
        snapshots(&snapshot_dir(&h, "db")).is_empty(),
        "a failed preBackup still produced a snapshot"
    );
}

// ---------------------------------------------------------------------------
// Retention
// ---------------------------------------------------------------------------

#[test]
fn retention_prunes_the_oldest_snapshots_from_disk_and_the_database() {
    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, b"payload").expect("write source");
    let mut s = spec("db", &source);
    s.retention = 2;
    // Sequence the names so the runs are distinguishable within one second.
    s.name_pattern = "{filename}".to_string();

    let mut kept = Vec::new();
    for i in 0..4 {
        s.name_pattern = format!("snapshot-{i}");
        kept.push(h.run(&s));
    }

    let on_disk = snapshots(&snapshot_dir(&h, "db"));
    assert_eq!(
        on_disk,
        vec!["snapshot-2".to_string(), "snapshot-3".to_string()],
        "retention kept the wrong snapshots"
    );

    let rows = h.store.backup_runs("db").expect("history");
    assert_eq!(rows.len(), 2, "pruned rows survived: {rows:?}");
    assert_eq!(rows[0].id, kept[3].id);
    assert_eq!(rows[1].id, kept[2].id);
}

#[test]
fn retention_of_one_keeps_only_the_newest_snapshot() {
    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, b"payload").expect("write source");
    let mut s = spec("db", &source);
    s.retention = 1;

    for i in 0..3 {
        s.name_pattern = format!("snapshot-{i}");
        h.run(&s);
    }

    assert_eq!(snapshots(&snapshot_dir(&h, "db")), vec!["snapshot-2"]);
    assert_eq!(h.store.backup_runs("db").expect("history").len(), 1);
}

#[test]
fn retention_prunes_directory_snapshots_too() {
    let h = Harness::new();
    let source = h.root.join("tree");
    std::fs::create_dir_all(&source).expect("tree");
    std::fs::write(source.join("a.txt"), b"a").expect("file");
    let mut s = spec("tree", &source);
    s.retention = 1;

    s.name_pattern = "snapshot-0".to_string();
    let first = h.run(&s);
    s.name_pattern = "snapshot-1".to_string();
    h.run(&s);

    let pruned = PathBuf::from(first.destination_path.expect("artifact"));
    assert!(!pruned.exists(), "pruned directory snapshot still on disk");
    assert_eq!(snapshots(&snapshot_dir(&h, "tree")), vec!["snapshot-1"]);
}

#[test]
fn failed_runs_do_not_evict_good_snapshots() {
    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, b"payload").expect("write source");
    let mut good = spec("db", &source);
    good.retention = 2;
    good.name_pattern = "snapshot-0".to_string();
    h.run(&good);
    good.name_pattern = "snapshot-1".to_string();
    h.run(&good);

    let mut broken = spec("db", &h.root.join("gone.db"));
    broken.retention = 2;
    for _ in 0..5 {
        h.run(&broken);
    }

    assert_eq!(
        snapshots(&snapshot_dir(&h, "db")),
        vec!["snapshot-0".to_string(), "snapshot-1".to_string()],
        "a burst of failures deleted good snapshots"
    );
}

#[test]
fn failed_run_history_is_bounded_by_retention() {
    let h = Harness::new();
    let mut s = spec("db", &h.root.join("gone.db"));
    s.retention = 2;

    for _ in 0..6 {
        h.run(&s);
    }

    let rows = h.store.backup_runs("db").expect("history");
    assert_eq!(rows.len(), 2, "failed-run rows grew unbounded: {rows:?}");
    assert!(rows.iter().all(|r| r.status == BackupRunStatus::Failed));
}

#[test]
fn retention_is_scoped_to_one_backup_name() {
    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, b"payload").expect("write source");
    let mut a = spec("alpha", &source);
    a.retention = 1;
    let mut b = spec("beta", &source);
    b.retention = 1;

    h.run(&a);
    h.run(&b);

    assert_eq!(h.store.backup_runs("alpha").expect("alpha").len(), 1);
    assert_eq!(h.store.backup_runs("beta").expect("beta").len(), 1);
    assert_eq!(snapshots(&snapshot_dir(&h, "alpha")).len(), 1);
    assert_eq!(snapshots(&snapshot_dir(&h, "beta")).len(), 1);
}

#[test]
fn a_manually_deleted_snapshot_still_has_its_row_pruned() {
    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, b"payload").expect("write source");
    let mut s = spec("db", &source);
    s.retention = 1;
    s.name_pattern = "snapshot-0".to_string();
    let first = h.run(&s);

    std::fs::remove_file(first.destination_path.clone().expect("artifact")).expect("manual delete");

    s.name_pattern = "snapshot-1".to_string();
    h.run(&s);

    let rows = h.store.backup_runs("db").expect("history");
    assert_eq!(rows.len(), 1, "row for a vanished snapshot was not pruned");
    assert_eq!(
        rows[0]
            .destination_path
            .as_deref()
            .map(Path::new)
            .map(Path::to_path_buf),
        Some(snapshot_dir(&h, "db").join("snapshot-1"))
    );
}

// ---------------------------------------------------------------------------
// Pruning containment
// ---------------------------------------------------------------------------

/// Plant a row whose `destination_path` names `victim`, then run enough
/// backups to push it past retention. Nothing outside the destination may be
/// touched, whatever the DB says.
fn prune_with_planted_row(h: &Harness, victim: &Path) -> BackupRunRecord {
    let planted = h
        .store
        .record_backup_run(&BackupRunDraft {
            name: "db".to_string(),
            source: "/var/lib/app/data.db".to_string(),
            destination_path: Some(crate::to_posix_string(victim)),
            size_bytes: Some(1),
            status: BackupRunStatus::Success,
            error: None,
            started_at: "2026-01-01T00:00:00Z".to_string(),
            finished_at: "2026-01-01T00:00:01Z".to_string(),
        })
        .expect("plant the row");

    let source = h.root.join("data.db");
    std::fs::write(&source, b"payload").expect("write source");
    let mut s = spec("db", &source);
    s.retention = 1;
    s.name_pattern = "snapshot-0".to_string();
    h.run(&s);
    planted
}

#[test]
fn pruning_never_deletes_a_recorded_path_outside_the_destination() {
    let h = Harness::new();
    let victim = h.root.join("precious.txt");
    std::fs::write(&victim, b"not-a-snapshot").expect("write victim");

    let planted = prune_with_planted_row(&h, &victim);

    assert!(
        victim.exists(),
        "pruning deleted a path outside the destination"
    );
    assert_eq!(
        std::fs::read(&victim).expect("victim readable"),
        b"not-a-snapshot"
    );
    // The row is dropped so a stale entry cannot re-warn forever, but the file
    // it named is left for the operator.
    let rows = h.store.backup_runs("db").expect("history");
    assert!(
        !rows.iter().any(|r| r.id == planted.id),
        "the out-of-destination row was kept: {rows:?}"
    );
}

#[test]
fn pruning_never_recursively_deletes_a_directory_outside_the_destination() {
    let h = Harness::new();
    let victim = h.root.join("precious-tree");
    std::fs::create_dir_all(victim.join("nested")).expect("victim tree");
    std::fs::write(victim.join("nested/data.txt"), b"keep me").expect("victim file");

    prune_with_planted_row(&h, &victim);

    assert!(
        victim.join("nested/data.txt").exists(),
        "pruning recursively deleted a directory outside the destination"
    );
}

#[test]
fn an_out_of_destination_row_does_not_consume_a_retention_slot() {
    let h = Harness::new();
    let victim = h.root.join("precious.txt");
    std::fs::write(&victim, b"not-a-snapshot").expect("write victim");

    prune_with_planted_row(&h, &victim);

    // retention = 1 and one foreign row: the real snapshot must survive, not be
    // evicted by a row that names something this unit never wrote.
    assert_eq!(snapshots(&snapshot_dir(&h, "db")), vec!["snapshot-0"]);
    assert!(victim.exists());
}

#[test]
fn pruning_ignores_a_row_that_walks_out_through_a_relative_segment() {
    let h = Harness::new();
    let victim = h.root.join("precious.txt");
    std::fs::write(&victim, b"not-a-snapshot").expect("write victim");
    // A hand-edited row prefixed with the destination but escaping through `..`
    // — string containment would pass it; component checking must not. The
    // destination is created up front so the assertion below can resolve the
    // path: getting the `..` count wrong would aim the escape at nothing and
    // silently turn this into a test that proves nothing.
    let destination = snapshot_dir(&h, "db");
    std::fs::create_dir_all(&destination).expect("destination");
    let escaping = destination
        .join("..")
        .join("..")
        .join("..")
        .join("precious.txt");
    assert!(
        escaping.exists(),
        "the escape path must resolve to the victim for this test to mean anything"
    );

    prune_with_planted_row(&h, &escaping);

    assert!(victim.exists(), "a '..' row escaped the containment gate");
}

#[test]
fn pruning_removes_an_empty_directory_a_nested_pattern_left_behind() {
    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, b"payload").expect("write source");
    let mut s = spec("db", &source);
    s.retention = 1;

    s.name_pattern = "daily/first".to_string();
    h.run(&s);
    s.name_pattern = "weekly/second".to_string();
    h.run(&s);

    let dest = snapshot_dir(&h, "db");
    assert!(
        !dest.join("daily").exists(),
        "the emptied intermediate directory was left behind"
    );
    assert!(dest.join("weekly/second").exists());
}

#[test]
fn is_snapshot_within_admits_only_plain_descendants() {
    let dest = Path::new("/state/backups/db");
    assert!(is_snapshot_within(
        Path::new("/state/backups/db/snap"),
        dest
    ));
    assert!(is_snapshot_within(
        Path::new("/state/backups/db/daily/snap"),
        dest
    ));
    // The destination itself is not a snapshot.
    assert!(!is_snapshot_within(dest, dest));
    // A sibling whose name merely starts with the destination's.
    assert!(!is_snapshot_within(
        Path::new("/state/backups/db-old/snap"),
        dest
    ));
    assert!(!is_snapshot_within(Path::new("/etc/passwd"), dest));
    assert!(!is_snapshot_within(
        Path::new("/state/backups/db/../../precious"),
        dest
    ));
    assert!(!is_snapshot_within(Path::new("relative/snap"), dest));
}

#[test]
fn is_at_or_within_treats_equal_paths_as_contained() {
    let root = Path::new("/home/u/Pictures");
    assert!(is_at_or_within(root, root));
    assert!(is_at_or_within(Path::new("/home/u/Pictures/backups"), root));
    assert!(!is_at_or_within(Path::new("/home/u/Pictures-old"), root));
    assert!(!is_at_or_within(Path::new("/home/u"), root));
}

// ---------------------------------------------------------------------------
// Source / destination containment
// ---------------------------------------------------------------------------

#[test]
fn a_destination_inside_the_source_is_rejected_before_any_copy() {
    let h = Harness::new();
    let source = h.root.join("Pictures");
    std::fs::create_dir_all(&source).expect("source tree");
    std::fs::write(source.join("a.jpg"), b"jpeg").expect("source file");
    let mut s = spec("photos", &source);
    s.destination = Some(source.join("backups"));

    let record = h.run(&s);

    assert_eq!(record.status, BackupRunStatus::Failed);
    let error = record.error.clone().expect("failure detail");
    assert!(
        error.contains("is inside source"),
        "unhelpful error: {error}"
    );
    assert!(
        !source.join("backups").exists(),
        "the rejected destination was created anyway"
    );
    assert_eq!(
        std::fs::read_dir(&source).expect("source readable").count(),
        1,
        "the source tree was modified by a rejected backup"
    );
}

#[test]
fn a_destination_two_levels_inside_the_source_is_rejected() {
    let h = Harness::new();
    let source = h.root.join("Pictures");
    std::fs::create_dir_all(&source).expect("source tree");
    let mut s = spec("photos", &source);
    s.destination = Some(source.join("archive").join("backups"));

    let record = h.run(&s);

    assert_eq!(record.status, BackupRunStatus::Failed);
    assert!(!source.join("archive").exists());
}

#[test]
fn a_destination_equal_to_the_source_is_rejected() {
    let h = Harness::new();
    let source = h.root.join("Pictures");
    std::fs::create_dir_all(&source).expect("source tree");
    let mut s = spec("photos", &source);
    s.destination = Some(source.clone());

    let record = h.run(&s);

    assert_eq!(record.status, BackupRunStatus::Failed);
    assert!(
        record
            .error
            .unwrap_or_default()
            .contains("is inside source")
    );
}

#[test]
fn a_snapshot_path_that_would_clobber_the_source_is_rejected() {
    let h = Harness::new();
    let dest = h.root.join("backups");
    std::fs::create_dir_all(&dest).expect("dest");
    let source = dest.join("data.db");
    std::fs::write(&source, b"the only copy").expect("write source");
    let mut s = spec("db", &source);
    s.destination = Some(dest.clone());
    // Renders to exactly the source's own filename inside its own directory.
    s.name_pattern = "{filename}".to_string();

    let record = h.run(&s);

    assert_eq!(record.status, BackupRunStatus::Failed);
    let error = record.error.clone().expect("failure detail");
    assert!(error.contains("collides with source"), "got: {error}");
    assert_eq!(
        std::fs::read(&source).expect("source survives"),
        b"the only copy"
    );
}

#[cfg(unix)]
#[test]
fn a_destination_symlinked_into_the_source_is_rejected() {
    let h = Harness::new();
    let source = h.root.join("Pictures");
    std::fs::create_dir_all(&source).expect("source tree");
    std::fs::write(source.join("a.jpg"), b"jpeg").expect("source file");
    // Lexically disjoint from the source, physically inside it.
    let link = h.root.join("link");
    std::os::unix::fs::symlink(&source, &link).expect("symlink");
    let mut s = spec("photos", &source);
    s.destination = Some(link.join("backups"));

    let record = h.run(&s);

    assert_eq!(
        record.status,
        BackupRunStatus::Failed,
        "a destination that reaches the source through a symlink was accepted"
    );
    let error = record.error.clone().expect("failure detail");
    assert!(error.contains("is inside source"), "got: {error}");
    assert!(
        !source.join("backups").exists(),
        "the rejected destination was created through the link anyway"
    );
    assert_eq!(
        std::fs::read_dir(&source).expect("source readable").count(),
        1,
        "the source tree was modified by a rejected backup"
    );
}

#[cfg(unix)]
#[test]
fn a_source_symlinked_into_the_destination_is_rejected() {
    let h = Harness::new();
    let real = h.root.join("Pictures");
    std::fs::create_dir_all(&real).expect("source tree");
    let dest = real.join("backups");
    std::fs::create_dir_all(&dest).expect("dest");
    // The mirror image: the *source* is the aliased operand this time.
    let link = h.root.join("link");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    let mut s = spec("photos", &link);
    s.destination = Some(dest);

    let record = h.run(&s);

    assert_eq!(record.status, BackupRunStatus::Failed);
    assert!(
        record
            .error
            .unwrap_or_default()
            .contains("is inside source")
    );
}

// ---------------------------------------------------------------------------
// Permissions
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn directory_snapshots_carry_the_source_directory_modes() {
    use std::os::unix::fs::PermissionsExt;

    let h = Harness::new();
    let source = h.root.join("dotssh");
    std::fs::create_dir_all(source.join("private")).expect("tree");
    std::fs::write(source.join("private/id_ed25519"), b"key").expect("key");
    std::fs::set_permissions(
        source.join("private/id_ed25519"),
        std::fs::Permissions::from_mode(0o600),
    )
    .expect("chmod key");
    std::fs::set_permissions(
        source.join("private"),
        std::fs::Permissions::from_mode(0o700),
    )
    .expect("chmod inner");
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o700)).expect("chmod root");

    let record = h.run(&spec("dotssh", &source));

    let dest = PathBuf::from(record.destination_path.expect("artifact"));
    let mode = |p: &Path| {
        std::fs::metadata(p)
            .unwrap_or_else(|e| panic!("stat {}: {e}", p.posix()))
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode(&dest), 0o700, "snapshot root lost the source mode");
    assert_eq!(
        mode(&dest.join("private")),
        0o700,
        "nested directory lost the source mode"
    );
    assert_eq!(mode(&dest.join("private/id_ed25519")), 0o600);
}

#[cfg(unix)]
#[test]
fn the_default_destination_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, b"payload").expect("write source");

    h.run(&spec("db", &source));

    let mode = std::fs::metadata(snapshot_dir(&h, "db"))
        .expect("stat destination")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode, 0o700,
        "the default destination is group/world readable"
    );
}

#[cfg(unix)]
#[test]
fn an_explicit_destination_keeps_the_users_own_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, b"payload").expect("write source");
    let dest = h.root.join("shared");
    std::fs::create_dir_all(&dest).expect("dest");
    std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755)).expect("chmod dest");
    let mut s = spec("db", &source);
    s.destination = Some(dest.clone());

    h.run(&s);

    let mode = std::fs::metadata(&dest)
        .expect("stat destination")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o755, "cfgd re-chmodded a user-chosen destination");
}

// ---------------------------------------------------------------------------
// Snapshot naming
// ---------------------------------------------------------------------------

#[test]
fn snapshot_name_rejects_a_traversing_pattern() {
    let mut s = spec("db", Path::new("/var/lib/app/data.db"));
    s.name_pattern = "../escape".to_string();
    let err = snapshot_name(&s, Path::new("/var/lib/app/data.db"))
        .expect_err("traversal must be rejected");
    assert!(err.to_string().contains("'..'"), "got: {err}");
}

/// A `namePattern` rendering to a directory reference makes the snapshot target
/// a directory that already exists — clearing it to make way for the rename
/// would empty it first and fail afterwards.
#[test]
fn snapshot_name_rejects_every_directory_reference_pattern() {
    for pattern in [".", "a/.", "./x", "a/../b", "..", "a//b", "daily/"] {
        let mut s = spec("db", Path::new("/var/lib/app/data.db"));
        s.name_pattern = pattern.to_string();
        match snapshot_name(&s, Path::new("/var/lib/app/data.db")) {
            Err(_) => {}
            Ok(rendered) => panic!(
                "pattern {pattern:?} was accepted and rendered {}",
                rendered.posix()
            ),
        }
    }
}

#[test]
fn a_dot_name_pattern_leaves_every_retained_snapshot_intact() {
    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, b"payload").expect("write source");
    let mut s = spec("db", &source);
    s.name_pattern = "keeper".to_string();
    h.run(&s);
    assert_eq!(snapshots(&snapshot_dir(&h, "db")), vec!["keeper"]);

    s.name_pattern = ".".to_string();
    let record = h.run(&s);

    assert_eq!(record.status, BackupRunStatus::Failed);
    assert!(!record.has_artifact());
    assert_eq!(
        snapshots(&snapshot_dir(&h, "db")),
        vec!["keeper"],
        "a '.' pattern destroyed the destination's contents"
    );
    assert_eq!(
        std::fs::read(snapshot_dir(&h, "db").join("keeper")).expect("keeper survives"),
        b"payload"
    );
}

/// `:` is legal in a unix filename but not in a snapshot name, and the default
/// pattern interpolates the filename verbatim — so the rejection has to point at
/// the filename rather than reading as a complaint about a drive letter the user
/// never typed.
#[test]
fn a_colon_in_the_source_filename_is_reported_against_the_filename() {
    let source = Path::new("/home/u/notes:2026.md");
    let s = spec("notes", source);
    assert_eq!(s.name_pattern, "{filename}.{timestamp}");

    let err = snapshot_name(&s, source)
        .expect_err("a colon in the rendered name must be rejected, not rewritten")
        .to_string();

    // Asserted as the labelled clause, not a bare substring: the rendered name
    // embeds the filename, so `contains("notes:2026.md")` would pass even with
    // the filename dropped from the error.
    assert!(
        err.contains("{filename} was 'notes:2026.md'"),
        "no filename attribution in: {err}"
    );
    assert!(
        err.contains("set an explicit namePattern"),
        "no remedy in: {err}"
    );
    assert!(err.contains("':'"), "no cause in: {err}");
}

#[test]
fn an_explicit_name_pattern_works_around_a_colon_in_the_filename() {
    let source = Path::new("/home/u/notes:2026.md");
    let mut s = spec("notes", source);
    s.name_pattern = "{name}.{timestamp}".to_string();

    let rendered = snapshot_name(&s, source).expect("a pattern without {filename} must render");

    let name = crate::to_posix_string(&rendered);
    assert!(name.starts_with("notes."), "{name}");
}

#[test]
fn snapshot_name_rejects_an_absolute_pattern() {
    let mut s = spec("db", Path::new("/var/lib/app/data.db"));
    s.name_pattern = "/etc/passwd".to_string();
    let err = snapshot_name(&s, Path::new("/var/lib/app/data.db"))
        .expect_err("absolute pattern must be rejected");
    assert!(err.to_string().contains("absolute"), "got: {err}");
}

#[test]
fn snapshot_name_rejects_an_empty_pattern() {
    let mut s = spec("db", Path::new("/var/lib/app/data.db"));
    s.name_pattern = "  ".to_string();
    let err = snapshot_name(&s, Path::new("/var/lib/app/data.db"))
        .expect_err("empty pattern must be rejected");
    assert!(err.to_string().contains("empty"), "got: {err}");
}

#[test]
fn snapshot_name_falls_back_to_the_backup_name_when_the_source_has_no_filename() {
    let s = spec("rootfs", Path::new("/"));
    let rendered = snapshot_name(&s, Path::new("/")).expect("root source must render");
    assert!(
        rendered.to_string_lossy().starts_with("rootfs."),
        "got: {}",
        rendered.posix()
    );
}

#[test]
fn a_nested_pattern_creates_the_intermediate_directories() {
    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, b"payload").expect("write source");
    let mut s = spec("db", &source);
    s.name_pattern = "daily/{filename}".to_string();

    let record = h.run(&s);

    let path = PathBuf::from(record.destination_path.expect("artifact"));
    assert_eq!(path, snapshot_dir(&h, "db").join("daily").join("data.db"));
    assert!(path.exists());
}

// ---------------------------------------------------------------------------
// One writer per unit
// ---------------------------------------------------------------------------

/// A `preBackup` hook that marks `started`, then blocks long enough for another
/// run to attempt the same unit while this one is mid-flight.
fn slow_start_hook(started: &Path) -> ScriptEntry {
    // native-ok: the path is interpolated into a shell command for THIS host.
    #[cfg(unix)]
    let run = format!("touch '{}'; sleep 2", started.display());
    #[cfg(windows)]
    let run = format!(
        "type nul > \"{}\" & ping -n 3 127.0.0.1 > nul",
        started.display()
    );
    hook(&run)
}

/// Run one unit against a FILE-backed store in `state_dir` — the shape two
/// concurrent runs need, since each owns its own connection.
fn run_against_dir(
    spec: &BackupSpec,
    home: &Path,
    config_dir: &Path,
    state_dir: &Path,
) -> Result<BackupRunRecord> {
    crate::with_test_home(home, || {
        let store = StateStore::open_in_dir(state_dir).expect("file-backed store");
        let (printer, _) = Printer::for_test();
        let unit = BackupUnit::new(spec, config_dir, "workstation", state_dir);
        run_backup(&unit, &store, &printer)
    })
}

fn busy_holder(err: &crate::errors::CfgdError) -> String {
    match err {
        crate::errors::CfgdError::Backup(BackupError::Busy { holder, .. }) => holder.clone(),
        other => panic!("expected a Busy error, got: {other}"),
    }
}

#[test]
fn a_run_is_refused_while_the_unit_lock_is_held() {
    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, b"payload").expect("write source");
    let s = spec("db", &source);

    let _held = crate::acquire_backup_lock(&h.state_dir(), "db").expect("take the unit lock");

    let config_dir = h.config_dir();
    let state_dir = h.state_dir();
    let err = crate::with_test_home(&h.root, || {
        let unit = BackupUnit::new(&s, &config_dir, "workstation", &state_dir);
        run_backup(&unit, &h.store, &h.printer).expect_err("a held unit lock must refuse the run")
    });

    assert!(
        busy_holder(&err).contains("pid"),
        "the refusal must name the holder: {err}"
    );
    assert!(
        h.store.latest_backup_run("db").expect("query").is_none(),
        "a refused run is not a run — nothing may be recorded"
    );
    assert!(
        !snapshot_dir(&h, "db").exists(),
        "a refused run must not touch the destination"
    );
}

#[test]
fn the_unit_lock_is_released_when_the_run_finishes() {
    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, b"payload").expect("write source");
    let s = spec("db", &source);

    let first = h.run(&s);
    assert_eq!(first.status, BackupRunStatus::Success);
    // A second run proves the guard is not a one-shot: if the lock leaked, this
    // is where every subsequent scheduled fire would start failing.
    let second = h.run(&s);
    assert_eq!(second.status, BackupRunStatus::Success);
}

#[test]
fn two_different_units_do_not_block_each_other() {
    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, b"payload").expect("write source");

    let _held =
        crate::acquire_backup_lock(&h.state_dir(), "other").expect("take another unit lock");

    let record = h.run(&spec("db", &source));
    assert_eq!(
        record.status,
        BackupRunStatus::Success,
        "the lock is per unit, not global: {:?}",
        record.error
    );
}

#[test]
fn a_concurrent_run_of_one_unit_is_refused_and_the_in_flight_snapshot_stays_whole() {
    let h = Harness::new();
    let source = h.root.join("tree");
    std::fs::create_dir_all(source.join("nested")).expect("source tree");
    std::fs::write(source.join("one.txt"), b"first").expect("write");
    std::fs::write(source.join("nested/two.txt"), b"second").expect("write");

    let started = h.root.join("first-run-started");
    let mut slow = spec("db", &source);
    slow.pre_backup = vec![slow_start_hook(&started)];

    let home = h.root.clone();
    let config_dir = h.config_dir();
    let state_dir = h.state_dir();
    let slow_spec = slow.clone();
    let first = std::thread::spawn(move || {
        run_against_dir(&slow_spec, &home, &config_dir, &state_dir).expect("first run recorded")
    });

    // Sync point: the hook has fired, so the first run holds the unit lock and
    // has not reached the copy yet.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !started.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the first run's preBackup hook never started"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    let quick = spec("db", &source);
    let err = run_against_dir(&quick, &h.root, &h.config_dir(), &h.state_dir())
        .expect_err("a second run of the SAME unit must be refused, not interleaved");
    assert!(busy_holder(&err).contains("pid"), "got: {err}");

    let record = first.join().expect("first run thread");
    assert_eq!(
        record.status,
        BackupRunStatus::Success,
        "the refused run must not have disturbed the one in flight: {:?}",
        record.error
    );

    // The whole tree landed, and no staging directory survived — the torn
    // half-copy this lock exists to prevent would show up as either a missing
    // file or a leftover `.db.partial`.
    let snapshot = PathBuf::from(record.destination_path.expect("artifact"));
    assert_eq!(
        std::fs::read_to_string(snapshot.join("one.txt")).expect("one.txt"),
        "first"
    );
    assert_eq!(
        std::fs::read_to_string(snapshot.join("nested/two.txt")).expect("two.txt"),
        "second"
    );
    let leftovers: Vec<String> = snapshots(&snapshot_dir(&h, "db"))
        .into_iter()
        .filter(|n| n.ends_with(".partial"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "a staging tree survived the run: {leftovers:?}"
    );

    let store = StateStore::open_in_dir(&h.state_dir()).expect("store");
    assert_eq!(
        store.backup_runs("db").expect("history").len(),
        1,
        "the refused run must not have recorded a row"
    );
}
