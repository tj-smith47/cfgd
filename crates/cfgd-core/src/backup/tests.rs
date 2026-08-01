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
fn pre_hook_failure_aborts_the_unit_and_records_a_failed_run() {
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
    assert!(
        !marker.exists(),
        "postBackup ran even though preBackup aborted the unit"
    );
    assert!(
        snapshots(&snapshot_dir(&h, "db")).is_empty(),
        "an aborted unit wrote a snapshot"
    );
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
fn a_continue_on_error_pre_hook_still_aborts_the_unit() {
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

    // continueOnError governs the rest of the hook LIST, not the unit: the
    // second hook runs, but the recorded failure still aborts the snapshot.
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
