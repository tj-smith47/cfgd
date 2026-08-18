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

/// An inline hook that appends one `x` to `path`, so a test can count how many
/// times a hook list ran rather than only that it ran.
fn append_hook(path: &Path) -> ScriptEntry {
    // native-ok: the path is interpolated into a shell command for THIS host.
    #[cfg(unix)]
    let run = format!("printf 'x' >> '{}'", path.display());
    #[cfg(windows)]
    let run = format!("echo x>> \"{}\"", path.display());
    hook(&run)
}

/// How many times [`append_hook`] ran, counted from its tally file.
fn hook_runs(path: &Path) -> usize {
    std::fs::read_to_string(path)
        .expect("tally file should exist")
        .matches('x')
        .count()
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

    /// A file under the harness root with `bytes` in it — the source most
    /// backup tests point a unit at.
    fn seed_file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.root.join(name);
        std::fs::write(&path, bytes).expect("write source");
        path
    }

    /// Run `spec` with the harness's real config/state dirs under an isolated
    /// `$HOME`, so hook working directories and `~` expansion never touch the
    /// developer's home.
    fn run(&self, spec: &BackupSpec) -> BackupRunRecord {
        self.run_with_items(spec).0
    }

    /// [`Self::run`] plus the per-line items the run pushed — what the
    /// `Backups` pseudo-phase's rollup counts.
    fn run_with_items(&self, spec: &BackupSpec) -> (BackupRunRecord, Vec<BackupItem>) {
        let config_dir = self.config_dir();
        let state_dir = self.state_dir();
        crate::with_test_home(&self.root, || {
            let unit = BackupUnit::new(spec, &config_dir, "workstation", &state_dir);
            let mut items = Vec::new();
            let record = run_backup(&unit, &self.store, &self.printer, &mut items)
                .expect("run must be recorded");
            (record, items)
        })
    }

    /// Every restorable snapshot of `spec`, newest first.
    fn snapshots_of(&self, spec: &BackupSpec) -> Vec<SnapshotInfo> {
        let config_dir = self.config_dir();
        let state_dir = self.state_dir();
        crate::with_test_home(&self.root, || {
            let unit = BackupUnit::new(spec, &config_dir, "workstation", &state_dir);
            list_snapshots(&unit, &self.store).expect("snapshot list")
        })
    }

    /// Select and restore in one step, the way the CLI does.
    fn restore(
        &self,
        spec: &BackupSpec,
        at: Option<&str>,
        to: Option<&Path>,
    ) -> Result<RestoreOutcome> {
        let config_dir = self.config_dir();
        let state_dir = self.state_dir();
        crate::with_test_home(&self.root, || {
            let unit = BackupUnit::new(spec, &config_dir, "workstation", &state_dir);
            let snapshots = list_snapshots(&unit, &self.store)?;
            let selected = select_snapshot(&spec.name, &snapshots, at)?;
            restore_backup(&unit, &self.store, &self.printer, selected, to)
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

// ---------------------------------------------------------------------------
// Copy: file sources
// ---------------------------------------------------------------------------

#[test]
fn file_source_is_snapshotted_and_recorded() {
    let h = Harness::new();
    let source = h.seed_file("data.db", b"payload-bytes");

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
    let source = h.seed_file("data.db", b"x");
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
    let source = h.seed_file("data.db", b"x");
    let mut s = spec("db", &source);
    let dest = h.root.join("elsewhere").join("nested");
    s.destination = Some(dest.clone());

    let record = h.run(&s);

    let path = PathBuf::from(record.destination_path.expect("artifact"));
    assert_eq!(path.parent(), Some(dest.as_path()));
    assert!(path.exists());
}

#[test]
fn a_same_second_rerun_keeps_both_colliding_snapshots() {
    let h = Harness::new();
    let source = h.seed_file("data.db", b"first");
    let mut s = spec("db", &source);
    // Drop {timestamp} so both runs render the identical name — the same
    // collision `{timestamp}`'s one-second resolution produces for real.
    s.name_pattern = "{filename}".to_string();

    let first = h.run(&s);
    std::fs::write(&source, b"second-and-longer").expect("rewrite source");
    let second = h.run(&s);

    let first_path = PathBuf::from(first.destination_path.expect("first artifact"));
    let second_path = PathBuf::from(second.destination_path.expect("second artifact"));
    assert_ne!(
        first_path, second_path,
        "the second run overwrote the first snapshot instead of taking a free name"
    );
    assert_eq!(
        second_path.file_name().and_then(|n| n.to_str()),
        Some("data.db-1")
    );
    assert_eq!(
        std::fs::read(&first_path).expect("first readable"),
        b"first"
    );
    assert_eq!(
        std::fs::read(&second_path).expect("second readable"),
        b"second-and-longer"
    );
    assert_eq!(second.size_bytes, Some(17));

    // Both rows survive with distinct payloads, so neither prune can delete a
    // payload the other still claims.
    let listed = h.snapshots_of(&s);
    let names: Vec<&str> = listed.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["data.db-1", "data.db"]);
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
    let source = h.seed_file("data.db", b"payload");
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
    let source = h.seed_file("data.db", b"payload");
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
    let source = h.seed_file("data.db", b"payload");
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
    let source = h.seed_file("data.db", b"payload");
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
    let source = h.seed_file("data.db", b"payload");
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
    let source = h.seed_file("data.db", b"payload");
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
    let source = h.seed_file("data.db", b"payload");
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
    let source = h.seed_file("data.db", b"payload");
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
    let source = h.seed_file("data.db", b"payload");
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
    let source = h.seed_file("data.db", b"payload");
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
    let source = h.seed_file("data.db", b"payload");
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

    let source = h.seed_file("data.db", b"payload");
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
    let source = h.seed_file("data.db", b"payload");
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
    let source = h.seed_file("data.db", b"payload");

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
    let source = h.seed_file("data.db", b"payload");
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
    let source = h.seed_file("data.db", b"payload");
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
    assert!(
        err.to_string().contains("starts from a filesystem root"),
        "got: {err}"
    );
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
    let source = h.seed_file("data.db", b"payload");
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
        run_backup(&unit, &store, &printer, &mut Vec::new())
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
    let source = h.seed_file("data.db", b"payload");
    let s = spec("db", &source);

    let _held = crate::acquire_backup_lock(&h.state_dir(), "db").expect("take the unit lock");

    let config_dir = h.config_dir();
    let state_dir = h.state_dir();
    let err = crate::with_test_home(&h.root, || {
        let unit = BackupUnit::new(&s, &config_dir, "workstation", &state_dir);
        run_backup(&unit, &h.store, &h.printer, &mut Vec::new())
            .expect_err("a held unit lock must refuse the run")
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
    let source = h.seed_file("data.db", b"payload");
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
    let source = h.seed_file("data.db", b"payload");

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
        // sleep-ok: bounded deadline poll on a filesystem side effect, not a fixed-duration guess
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

// ---------------------------------------------------------------------------
// Restore: snapshot listing and selection
// ---------------------------------------------------------------------------

/// Write a snapshot payload and the run record that owns it.
///
/// Hand-seeded rather than taken with [`run_backup`] because the engine stamps
/// names to the second: two real runs inside one test render the same name, and
/// the ordering these tests are about would depend on how long the suite took.
fn seed_snapshot(
    h: &Harness,
    name: &str,
    snapshot: &str,
    finished_at: &str,
    body: &str,
) -> PathBuf {
    let path = snapshot_dir(h, name).join(snapshot);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("snapshot parent");
    }
    std::fs::write(&path, body).expect("snapshot payload");
    h.store
        .record_backup_run(&BackupRunDraft {
            name: name.to_string(),
            source: "/src".to_string(),
            destination_path: Some(crate::to_posix_string(&path)),
            size_bytes: Some(body.len() as u64),
            status: BackupRunStatus::Success,
            error: None,
            started_at: finished_at.to_string(),
            finished_at: finished_at.to_string(),
        })
        .expect("seed run record");
    path
}

/// Names of any restore-staging directories left under `dir`.
fn staging_leftovers(dir: &Path) -> Vec<String> {
    snapshots(dir)
        .into_iter()
        .filter(|n| n.starts_with(".cfgd-restore-"))
        .collect()
}

#[test]
fn list_snapshots_reports_newest_first_with_destination_relative_names() {
    let h = Harness::new();
    let s = spec("db", Path::new("/src"));
    seed_snapshot(
        &h,
        "db",
        "db.20260730T120000Z",
        "2026-07-30T12:00:00Z",
        "old",
    );
    seed_snapshot(
        &h,
        "db",
        "db.20260801T231502Z",
        "2026-08-01T23:15:02Z",
        "newer",
    );

    let listed = h.snapshots_of(&s);
    assert_eq!(
        listed.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        ["db.20260801T231502Z", "db.20260730T120000Z"],
        "newest first, named relative to the destination"
    );
    assert_eq!(listed[0].created, "2026-08-01T23:15:02Z");
    assert_eq!(listed[0].size_bytes, 5);
}

#[test]
fn list_snapshots_names_a_nested_pattern_relative_to_the_destination() {
    let h = Harness::new();
    let s = spec("db", Path::new("/src"));
    seed_snapshot(
        &h,
        "db",
        "daily/db.20260801T000000Z",
        "2026-08-01T00:00:00Z",
        "x",
    );

    let listed = h.snapshots_of(&s);
    assert_eq!(listed[0].name, "daily/db.20260801T000000Z");
}

#[test]
fn list_snapshots_ignores_a_record_pointing_outside_the_destination() {
    let h = Harness::new();
    let s = spec("db", Path::new("/src"));
    let stray = h.root.join("elsewhere.snap");
    std::fs::write(&stray, "foreign").expect("stray file");
    h.store
        .record_backup_run(&BackupRunDraft {
            name: "db".to_string(),
            source: "/src".to_string(),
            destination_path: Some(crate::to_posix_string(&stray)),
            size_bytes: Some(7),
            status: BackupRunStatus::Success,
            error: None,
            started_at: "2026-08-01T00:00:00Z".to_string(),
            finished_at: "2026-08-01T00:00:00Z".to_string(),
        })
        .expect("stray record");

    assert!(
        h.snapshots_of(&s).is_empty(),
        "a row outside this unit's destination must never be offered as a restore source"
    );
    assert!(stray.exists(), "listing must not touch the stray path");
}

#[test]
fn list_snapshots_ignores_a_record_whose_payload_is_gone() {
    let h = Harness::new();
    let s = spec("db", Path::new("/src"));
    let path = seed_snapshot(&h, "db", "db.20260801T000000Z", "2026-08-01T00:00:00Z", "x");
    std::fs::remove_file(&path).expect("remove payload");

    assert!(
        h.snapshots_of(&s).is_empty(),
        "a snapshot that cannot be restored is not one"
    );
}

#[test]
fn list_snapshots_ignores_a_failed_run_with_no_artifact() {
    let h = Harness::new();
    let s = spec("db", &h.root.join("never-created"));
    h.run(&s);

    assert!(
        h.snapshots_of(&s).is_empty(),
        "a failed run records no artifact, so it lists no snapshot"
    );
}

/// The two-snapshot fixture the selection tests share: one older, one newer.
fn seed_old_and_new(h: &Harness) {
    seed_snapshot(
        h,
        "db",
        "db.20260730T120000Z",
        "2026-07-30T12:00:00Z",
        "old",
    );
    seed_snapshot(
        h,
        "db",
        "db.20260801T231502Z",
        "2026-08-01T23:15:02Z",
        "new",
    );
}

#[test]
fn select_snapshot_defaults_to_the_newest() {
    let h = Harness::new();
    let s = spec("db", Path::new("/src"));
    seed_old_and_new(&h);

    let listed = h.snapshots_of(&s);
    let chosen = select_snapshot("db", &listed, None).expect("newest");
    assert_eq!(chosen.name, "db.20260801T231502Z");
}

#[test]
fn select_snapshot_accepts_a_full_name_and_a_timestamp_fragment() {
    let h = Harness::new();
    let s = spec("db", Path::new("/src"));
    seed_old_and_new(&h);
    let listed = h.snapshots_of(&s);

    let by_name = select_snapshot("db", &listed, Some("db.20260730T120000Z")).expect("by name");
    assert_eq!(by_name.name, "db.20260730T120000Z");

    let by_stamp = select_snapshot("db", &listed, Some("20260730T120000Z")).expect("by timestamp");
    assert_eq!(
        by_stamp.name, "db.20260730T120000Z",
        "the timestamp portion alone must reach the same snapshot"
    );
}

#[test]
fn select_snapshot_rejects_an_unknown_name_and_lists_the_alternatives() {
    let h = Harness::new();
    let s = spec("db", Path::new("/src"));
    seed_snapshot(
        &h,
        "db",
        "db.20260801T231502Z",
        "2026-08-01T23:15:02Z",
        "new",
    );
    let listed = h.snapshots_of(&s);

    let err = select_snapshot("db", &listed, Some("20991231T000000Z"))
        .expect_err("an unknown snapshot must not silently fall back to the newest");
    match err {
        BackupError::SnapshotNotFound {
            requested,
            available,
            ..
        } => {
            assert_eq!(requested, "20991231T000000Z");
            assert_eq!(available, vec!["db.20260801T231502Z".to_string()]);
        }
        other => panic!("expected SnapshotNotFound, got {other:?}"),
    }
}

#[test]
fn select_snapshot_refuses_an_ambiguous_fragment() {
    let h = Harness::new();
    let s = spec("db", Path::new("/src"));
    seed_snapshot(&h, "db", "db.20260801T000000Z", "2026-08-01T00:00:00Z", "a");
    seed_snapshot(&h, "db", "db.20260802T000000Z", "2026-08-02T00:00:00Z", "b");
    let listed = h.snapshots_of(&s);

    let err = select_snapshot("db", &listed, Some("db."))
        .expect_err("a fragment matching two snapshots must never be guessed at");
    match err {
        BackupError::AmbiguousSnapshot { matches, .. } => assert_eq!(matches.len(), 2),
        other => panic!("expected AmbiguousSnapshot, got {other:?}"),
    }
}

#[test]
fn select_snapshot_rejects_an_empty_at_rather_than_calling_it_ambiguous() {
    let h = Harness::new();
    let s = spec("db", Path::new("/src"));
    seed_snapshot(&h, "db", "db.20260801T000000Z", "2026-08-01T00:00:00Z", "a");
    seed_snapshot(&h, "db", "db.20260802T000000Z", "2026-08-02T00:00:00Z", "b");
    let listed = h.snapshots_of(&s);

    let err = select_snapshot("db", &listed, Some("  ")).expect_err("an empty --at is unusable");
    assert!(
        matches!(err, BackupError::SnapshotNotFound { .. }),
        "got {err:?}"
    );
}

#[test]
fn select_snapshot_on_a_unit_that_has_never_run() {
    let err = select_snapshot("db", &[], None).expect_err("nothing to restore");
    assert!(
        matches!(err, BackupError::NoSnapshots { .. }),
        "got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Restore: the overlay
// ---------------------------------------------------------------------------

#[test]
fn restore_overlays_the_snapshot_and_leaves_extras_alone() {
    let h = Harness::new();
    let source = h.root.join("tree");
    std::fs::create_dir_all(source.join("nested")).expect("tree");
    std::fs::write(source.join("kept.txt"), "v1").expect("kept");
    std::fs::write(source.join("nested/deep.txt"), "d1").expect("deep");
    let s = spec("db", &source);
    h.run(&s);

    std::fs::write(source.join("kept.txt"), "clobbered").expect("clobber");
    std::fs::write(source.join("nested/deep.txt"), "clobbered").expect("clobber deep");
    std::fs::write(source.join("extra.txt"), "mine").expect("extra");

    let outcome = h.restore(&s, None, None).expect("restore");
    assert!(outcome.is_clean(), "outcome: {outcome:?}");
    assert_eq!(
        std::fs::read_to_string(source.join("kept.txt")).expect("kept"),
        "v1"
    );
    assert_eq!(
        std::fs::read_to_string(source.join("nested/deep.txt")).expect("deep"),
        "d1",
        "the overlay reaches nested files"
    );
    assert_eq!(
        std::fs::read_to_string(source.join("extra.txt")).expect("extra"),
        "mine",
        "a file the snapshot never held must survive the overlay"
    );
}

#[test]
fn restore_of_a_file_source_replaces_the_file() {
    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, "v1").expect("source");
    let s = spec("db", &source);
    h.run(&s);
    std::fs::write(&source, "v2").expect("clobber");

    let outcome = h.restore(&s, None, None).expect("restore");
    assert!(outcome.restored);
    assert_eq!(std::fs::read_to_string(&source).expect("restored"), "v1");
}

#[test]
fn restore_to_redirects_the_target_and_skips_the_safety_backup() {
    let h = Harness::new();
    let source = h.root.join("tree");
    std::fs::create_dir_all(&source).expect("tree");
    std::fs::write(source.join("a.txt"), "v1").expect("a");
    let s = spec("db", &source);
    h.run(&s);
    std::fs::write(source.join("a.txt"), "live").expect("live edit");
    let before = h.store.backup_runs("db").expect("history").len();

    let elsewhere = h.root.join("inspect");
    let outcome = h.restore(&s, None, Some(&elsewhere)).expect("restore --to");

    assert!(outcome.is_clean(), "outcome: {outcome:?}");
    assert!(
        outcome.safety_snapshot.is_none(),
        "--to leaves the live source untouched, so there is nothing to protect"
    );
    assert_eq!(
        std::fs::read_to_string(elsewhere.join("a.txt")).expect("redirected copy"),
        "v1"
    );
    assert_eq!(
        std::fs::read_to_string(source.join("a.txt")).expect("live source"),
        "live",
        "--to must not touch the unit's source"
    );
    assert_eq!(
        h.store.backup_runs("db").expect("history").len(),
        before,
        "no safety backup means no extra run record"
    );
}

#[test]
fn restore_to_source_takes_a_safety_backup_first() {
    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, "v1").expect("source");
    let s = spec("db", &source);
    h.run(&s);
    std::fs::write(&source, "live").expect("live edit");

    let outcome = h.restore(&s, None, None).expect("restore");
    let safety = outcome
        .safety_snapshot
        .as_deref()
        .expect("restoring over the live source must capture it first");
    assert_eq!(
        std::fs::read_to_string(Path::new(safety)).expect("safety snapshot"),
        "live",
        "the safety backup holds what the restore overwrote"
    );
    assert_eq!(std::fs::read_to_string(&source).expect("source"), "v1");
}

#[test]
fn restore_records_no_run_of_its_own_beyond_the_safety_backup() {
    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, "v1").expect("source");
    let s = spec("db", &source);
    h.run(&s);

    h.restore(&s, None, None).expect("restore");
    let runs = h.store.backup_runs("db").expect("history");
    assert_eq!(
        runs.len(),
        2,
        "one row for the original run, one for the safety backup — a restore records nothing"
    );
}

#[test]
fn the_safety_backup_never_replaces_the_snapshot_being_restored() {
    // A `namePattern` with no `{timestamp}` makes EVERY run render one name, so
    // the safety backup renders exactly the name of the snapshot being
    // restored — the worst case the collision suffix exists for.
    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, "v1").expect("source");
    let mut s = spec("db", &source);
    s.name_pattern = "latest".to_string();
    h.run(&s);
    std::fs::write(&source, "live").expect("live edit");

    let outcome = h.restore(&s, None, None).expect("restore");
    assert!(outcome.restored, "outcome: {outcome:?}");
    assert_eq!(std::fs::read_to_string(&source).expect("source"), "v1");
    assert_eq!(
        std::fs::read_to_string(snapshot_dir(&h, "db").join("latest")).expect("snapshot"),
        "v1",
        "the restored snapshot must still be on disk under its own name"
    );
    let safety = outcome.safety_snapshot.clone().expect("safety snapshot");
    assert!(
        safety.ends_with("latest-1"),
        "the safety backup must take a free name, got: {safety}"
    );
    assert_eq!(
        std::fs::read_to_string(&safety).expect("safety payload"),
        "live",
        "the safety snapshot must hold what the restore overwrote"
    );
}

#[test]
fn restore_aborts_when_the_safety_backup_produces_nothing() {
    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, "v1").expect("source");
    let mut s = spec("db", &source);
    h.run(&s);
    std::fs::write(&source, "live").expect("live edit");

    // An unusable `namePattern` makes the safety snapshot fail to write any
    // artifact — and a restore that cannot capture what it is about to destroy
    // must not run. The already-recorded snapshot is still selectable, because
    // selection reads rows rather than re-rendering the pattern.
    s.name_pattern = "../escape".to_string();
    let post_tally = h.root.join("post-tally");
    s.post_backup = vec![append_hook(&post_tally)];
    let err = h
        .restore(&s, None, None)
        .expect_err("an uncaptured source must not be overwritten");
    assert_eq!(
        hook_runs(&post_tally),
        1,
        "the abort still has to restart whatever preBackup stopped"
    );
    assert!(
        format!("{err}").contains("safety backup"),
        "expected the safety-backup refusal, got: {err}"
    );
    assert_eq!(
        std::fs::read_to_string(&source).expect("source"),
        "live",
        "the source must be exactly as the caller left it"
    );
    assert!(
        staging_leftovers(&h.root).is_empty(),
        "staging must be cleaned up on the abort path too"
    );
}

#[test]
fn restore_abort_carries_a_post_hook_failure_in_the_returned_error() {
    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, "v1").expect("source");
    let mut s = spec("db", &source);
    h.run(&s);
    std::fs::write(&source, "live").expect("live edit");

    // Same fatal abort as above, but the postBackup hook ALSO fails on the
    // way out. Both failures must reach the returned error itself — a stderr
    // status line never reaches a `-o json` consumer.
    s.name_pattern = "../escape".to_string();
    s.post_backup = vec![hook("exit 9")];
    let err = h
        .restore(&s, None, None)
        .expect_err("an uncaptured source must not be overwritten");
    let rendered = format!("{err}");
    assert!(
        rendered.contains("safety backup"),
        "the abort must stay the primary condition, got: {rendered}"
    );
    assert!(
        rendered.contains("postBackup"),
        "the post-hook failure must not vanish from the error, got: {rendered}"
    );
    assert_eq!(
        std::fs::read_to_string(&source).expect("source"),
        "live",
        "the source must be exactly as the caller left it"
    );
}

#[test]
fn restore_of_a_missing_source_skips_the_safety_backup() {
    let h = Harness::new();
    let source = h.root.join("tree");
    std::fs::create_dir_all(&source).expect("tree");
    std::fs::write(source.join("a.txt"), "v1").expect("a");
    let s = spec("db", &source);
    h.run(&s);
    std::fs::remove_dir_all(&source).expect("wipe the source");

    let outcome = h.restore(&s, None, None).expect("bare-metal restore");
    assert!(outcome.is_clean(), "outcome: {outcome:?}");
    assert!(
        outcome.safety_snapshot.is_none(),
        "there is nothing to protect when the source is gone"
    );
    assert_eq!(
        std::fs::read_to_string(source.join("a.txt")).expect("restored"),
        "v1"
    );
}

#[test]
fn restore_refuses_a_target_inside_the_snapshot_destination() {
    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, "v1").expect("source");
    let s = spec("db", &source);
    h.run(&s);

    let inside = snapshot_dir(&h, "db").join("victim");
    let err = h
        .restore(&s, None, Some(&inside))
        .expect_err("restoring into the snapshot store must be refused");
    assert!(
        format!("{err}").contains("snapshot destination"),
        "got: {err}"
    );
    assert!(
        !inside.exists(),
        "nothing may be written before the refusal"
    );
}

#[test]
fn restore_refuses_a_kind_mismatch_before_touching_the_target() {
    let h = Harness::new();
    let source = h.root.join("tree");
    std::fs::create_dir_all(&source).expect("tree");
    std::fs::write(source.join("a.txt"), "v1").expect("a");
    let s = spec("db", &source);
    h.run(&s);

    // A directory snapshot published over a FILE target would delete the file
    // on the way to the rename — well past overlay semantics.
    let file_target = h.root.join("occupied");
    std::fs::write(&file_target, "do not delete me").expect("occupied");
    let err = h
        .restore(&s, None, Some(&file_target))
        .expect_err("a directory snapshot must not be forced onto a file");
    assert!(format!("{err}").contains("directory"), "got: {err}");
    assert_eq!(
        std::fs::read_to_string(&file_target).expect("target"),
        "do not delete me"
    );
    assert!(staging_leftovers(&h.root).is_empty());
}

#[test]
fn restore_runs_the_units_hooks_around_the_mutation() {
    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, "v1").expect("source");
    let mut s = spec("db", &source);
    h.run(&s);

    let pre = h.root.join("pre.marker");
    let post = h.root.join("post.marker");
    s.pre_backup = vec![touch_hook(&pre)];
    s.post_backup = vec![touch_hook(&post)];

    let outcome = h.restore(&s, None, None).expect("restore");
    assert!(outcome.is_clean(), "outcome: {outcome:?}");
    assert!(pre.exists(), "preBackup must run before the restore");
    assert!(post.exists(), "postBackup must run after it");
}

#[test]
fn restore_skips_the_overlay_when_prebackup_fails_but_still_runs_postbackup() {
    let h = Harness::new();
    let source = h.root.join("tree");
    std::fs::create_dir_all(&source).expect("tree");
    std::fs::write(source.join("a.txt"), "v1").expect("a");
    let s = spec("db", &source);
    h.run(&s);
    std::fs::write(source.join("a.txt"), "live").expect("live edit");

    // `--to` keeps the safety backup out of the way, so this exercises the
    // RESTORE's own hook envelope rather than the safety backup's.
    let mut hooked = spec("db", &source);
    let post = h.root.join("post.marker");
    hooked.pre_backup = vec![hook("exit 4")];
    hooked.post_backup = vec![touch_hook(&post)];

    let elsewhere = h.root.join("inspect");
    let outcome = h
        .restore(&hooked, None, Some(&elsewhere))
        .expect("a hook failure is reported through the outcome, not as Err");
    assert!(
        !outcome.restored,
        "a failed preBackup must skip the overlay"
    );
    assert!(!outcome.is_clean());
    assert!(
        outcome.error.unwrap_or_default().contains("preBackup"),
        "the hook failure must reach the outcome"
    );
    assert!(
        !elsewhere.exists(),
        "nothing may be written when the overlay is skipped"
    );
    assert!(post.exists(), "postBackup runs on every path");
    assert!(staging_leftovers(&h.root).is_empty());
}

#[test]
fn restore_leaves_no_staging_directory_behind_on_success() {
    let h = Harness::new();
    let source = h.root.join("tree");
    std::fs::create_dir_all(&source).expect("tree");
    std::fs::write(source.join("a.txt"), "v1").expect("a");
    let s = spec("db", &source);
    h.run(&s);

    h.restore(&s, None, None).expect("restore");
    assert!(
        staging_leftovers(&h.root).is_empty(),
        "staging must not survive a successful restore"
    );
    assert!(
        staging_leftovers(&source).is_empty(),
        "and it must never land inside the restored tree"
    );
}

#[test]
fn restore_refuses_while_the_unit_is_locked_elsewhere() {
    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, "v1").expect("source");
    let s = spec("db", &source);
    h.run(&s);

    let state_dir = h.state_dir();
    let _held = crate::acquire_backup_lock(&state_dir, "db").expect("hold the unit lock");
    let err = h
        .restore(&s, None, None)
        .expect_err("a restore must not interleave with a run of the same unit");
    assert!(
        matches!(
            err,
            crate::errors::CfgdError::Backup(BackupError::Busy { .. })
        ),
        "got: {err}"
    );
}

#[cfg(unix)]
#[test]
fn restore_carries_the_snapshots_file_mode_back() {
    use std::os::unix::fs::PermissionsExt;

    let h = Harness::new();
    let source = h.root.join("tree");
    std::fs::create_dir_all(&source).expect("tree");
    let key = source.join("key");
    std::fs::write(&key, "secret").expect("key");
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).expect("chmod 600");
    let s = spec("db", &source);
    h.run(&s);

    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).expect("chmod 644");
    std::fs::write(&key, "leaked").expect("clobber");

    h.restore(&s, None, None).expect("restore");
    let mode = std::fs::metadata(&key)
        .expect("key metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode, 0o600,
        "a restore that widened a 0600 file to 0644 would leak what the mode protected"
    );
}

#[cfg(unix)]
#[test]
fn restore_leaves_a_symlink_in_the_source_untouched() {
    let h = Harness::new();
    let source = h.root.join("tree");
    std::fs::create_dir_all(&source).expect("tree");
    std::fs::write(source.join("a.txt"), "v1").expect("a");
    let s = spec("db", &source);
    h.run(&s);

    // The writer skips symlinks, so no snapshot holds one; a link that appears
    // in the source afterwards has no counterpart to overwrite it.
    let link = source.join("link");
    std::os::unix::fs::symlink("/etc/hostname", &link).expect("symlink");

    h.restore(&s, None, None).expect("restore");
    assert!(
        link.symlink_metadata().expect("link metadata").is_symlink(),
        "the restore must not resolve or replace a symlink it never captured"
    );
}

#[cfg(unix)]
#[test]
fn restore_replaces_a_symlink_at_a_name_the_snapshot_owns() {
    let h = Harness::new();
    let source = h.root.join("tree");
    std::fs::create_dir_all(&source).expect("tree");
    std::fs::write(source.join("a.txt"), "v1").expect("a");
    let s = spec("db", &source);
    h.run(&s);

    // The name the snapshot owns is replaced in the live target by a link
    // pointing OUTSIDE it — the shape that turns a plain `fs::copy` into a
    // write the safety backup never captured.
    let outsider = h.root.join("outside.txt");
    std::fs::write(&outsider, "do not touch").expect("outsider");
    std::fs::remove_file(source.join("a.txt")).expect("clear a.txt");
    std::os::unix::fs::symlink(&outsider, source.join("a.txt")).expect("symlink");

    h.restore(&s, None, None).expect("restore");

    assert_eq!(
        std::fs::read_to_string(&outsider).expect("outsider survives"),
        "do not touch",
        "the restore wrote THROUGH the link, outside the target"
    );
    let restored = source.join("a.txt");
    assert!(
        !restored
            .symlink_metadata()
            .expect("a.txt metadata")
            .is_symlink(),
        "the link must be replaced by the snapshot's own file"
    );
    assert_eq!(std::fs::read_to_string(&restored).expect("a.txt"), "v1");
}

#[cfg(unix)]
#[test]
fn restore_replaces_a_symlinked_directory_at_a_name_the_snapshot_owns() {
    let h = Harness::new();
    let source = h.root.join("tree");
    std::fs::create_dir_all(source.join("nested")).expect("tree");
    std::fs::write(source.join("nested/leaf.txt"), "v1").expect("leaf");
    let s = spec("db", &source);
    h.run(&s);

    let outsider = h.root.join("outside-dir");
    std::fs::create_dir_all(&outsider).expect("outsider dir");
    std::fs::remove_dir_all(source.join("nested")).expect("clear nested");
    std::os::unix::fs::symlink(&outsider, source.join("nested")).expect("dir symlink");

    h.restore(&s, None, None).expect("restore");

    assert!(
        !outsider.join("leaf.txt").exists(),
        "the restore wrote a whole subtree outside the target through a linked directory"
    );
    let nested = source.join("nested");
    assert!(
        !nested.symlink_metadata().expect("nested").is_symlink(),
        "the linked directory must be replaced by a real one"
    );
    assert_eq!(
        std::fs::read_to_string(nested.join("leaf.txt")).expect("leaf"),
        "v1"
    );
}

#[test]
fn restore_to_the_source_itself_still_takes_a_safety_backup() {
    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, "v1").expect("source");
    let s = spec("db", &source);
    h.run(&s);
    std::fs::write(&source, "live").expect("live edit");

    // `--to` aimed back at the source overwrites exactly what a plain restore
    // would; keying the skip on the flag rather than the path would destroy
    // "live" with nothing capturing it.
    let outcome = h
        .restore(&s, None, Some(&source))
        .expect("restore to the source");
    let safety = outcome.safety_snapshot.expect("safety snapshot");
    assert_eq!(
        std::fs::read_to_string(&safety).expect("safety payload"),
        "live"
    );
    assert_eq!(std::fs::read_to_string(&source).expect("source"), "v1");
}

#[test]
fn restore_to_a_path_inside_the_source_still_takes_a_safety_backup() {
    let h = Harness::new();
    let source = h.root.join("tree");
    std::fs::create_dir_all(&source).expect("tree");
    std::fs::write(source.join("a.txt"), "v1").expect("a");
    let s = spec("db", &source);
    h.run(&s);
    std::fs::write(source.join("a.txt"), "live").expect("live edit");

    let inside = source.join("restored-here");
    let outcome = h
        .restore(&s, None, Some(&inside))
        .expect("restore inside the source");
    let safety = outcome.safety_snapshot.expect("safety snapshot");
    assert_eq!(
        std::fs::read_to_string(PathBuf::from(&safety).join("a.txt")).expect("safety payload"),
        "live",
        "a target inside the source overwrites the source's own data"
    );
}

#[cfg(unix)]
#[test]
fn a_symlinked_source_round_trips_through_backup_and_restore() {
    let h = Harness::new();
    let real = h.root.join("real-dotfiles");
    std::fs::create_dir_all(&real).expect("real tree");
    std::fs::write(real.join("bashrc"), "v1").expect("bashrc");
    let link = h.root.join("dotfiles");
    std::os::unix::fs::symlink(&real, &link).expect("source symlink");

    // The writer stats the source through the link, so the snapshot is a
    // directory; the restore has to follow the same link or it would refuse the
    // unit it just backed up.
    let s = spec("db", &link);
    let record = h.run(&s);
    assert_eq!(record.status, BackupRunStatus::Success, "{record:?}");
    std::fs::write(real.join("bashrc"), "live").expect("live edit");

    let outcome = h.restore(&s, None, None).expect("restore through the link");
    assert!(outcome.restored, "outcome: {outcome:?}");
    assert_eq!(
        std::fs::read_to_string(real.join("bashrc")).expect("bashrc"),
        "v1"
    );
    assert_eq!(
        outcome.restored_to,
        crate::to_posix_string(real.canonicalize().expect("canonical real")),
        "the outcome must name where the bytes landed, not the link"
    );
    // The prompt, the declined payload, and this field are three separate
    // producers of one path; they must render it identically or a Windows
    // canonicalization shows up verbatim-prefixed in some of them and not
    // others.
    let config_dir = h.config_dir();
    let state_dir = h.state_dir();
    crate::with_test_home(&h.root, || {
        let unit = BackupUnit::new(&s, &config_dir, "workstation", &state_dir);
        assert_eq!(
            restore_target(&unit, None).resolved_display(),
            outcome.restored_to
        );
    });
    assert!(
        link.symlink_metadata().expect("link").is_symlink(),
        "the source link itself must survive the restore"
    );
}

#[test]
fn restore_hooks_see_the_restore_operation() {
    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, "v1").expect("source");
    let mut s = spec("db", &source);
    h.run(&s);

    let pre = h.root.join("pre-op");
    let post = h.root.join("post-op");
    s.pre_backup = vec![echo_env_hook(&["CFGD_PHASE", "CFGD_OPERATION"], &pre)];
    s.post_backup = vec![echo_env_hook(&["CFGD_PHASE", "CFGD_OPERATION"], &post)];

    h.restore(&s, None, None).expect("restore");

    assert_eq!(marker_contents(&pre), "preBackup:restore");
    assert_eq!(marker_contents(&post), "postBackup:restore");
}

#[test]
fn backup_hooks_see_the_backup_operation() {
    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, "v1").expect("source");
    let mut s = spec("db", &source);
    let pre = h.root.join("pre-op");
    s.pre_backup = vec![echo_env_hook(&["CFGD_OPERATION"], &pre)];

    h.run(&s);

    assert_eq!(marker_contents(&pre), "backup");
}

#[test]
fn restore_runs_each_hook_list_exactly_once_around_the_safety_backup() {
    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, "v1").expect("source");
    let mut s = spec("db", &source);
    h.run(&s);

    let pre_tally = h.root.join("pre-tally");
    let post_tally = h.root.join("post-tally");
    s.pre_backup = vec![append_hook(&pre_tally)];
    s.post_backup = vec![append_hook(&post_tally)];

    let outcome = h.restore(&s, None, None).expect("restore");
    assert!(
        outcome.safety_snapshot.is_some(),
        "this restore must take a safety backup for the count to mean anything"
    );
    assert_eq!(
        (hook_runs(&pre_tally), hook_runs(&post_tally)),
        (1, 1),
        "the safety snapshot must run inside the restore's hook envelope, not open its own"
    );
}

#[test]
fn a_failed_prebackup_skips_the_safety_backup_as_well_as_the_overlay() {
    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, "v1").expect("source");
    let mut s = spec("db", &source);
    h.run(&s);
    std::fs::write(&source, "live").expect("live edit");
    let before = snapshots(&snapshot_dir(&h, "db"));

    s.pre_backup = vec![hook("exit 4")];
    let outcome = h.restore(&s, None, None).expect("reported, not raised");

    assert!(!outcome.restored);
    assert!(
        outcome.safety_snapshot.is_none(),
        "a source the hook could not quiesce must not be snapshotted either"
    );
    assert_eq!(
        snapshots(&snapshot_dir(&h, "db")),
        before,
        "no new snapshot may appear"
    );
    assert_eq!(std::fs::read_to_string(&source).expect("source"), "live");
}

#[test]
fn restore_reports_a_snapshot_that_vanished_between_selection_and_the_lock() {
    let h = Harness::new();
    let source = h.root.join("data.db");
    std::fs::write(&source, "v1").expect("source");
    let s = spec("db", &source);
    h.run(&s);

    let config_dir = h.config_dir();
    let state_dir = h.state_dir();
    let err = crate::with_test_home(&h.root, || {
        let unit = BackupUnit::new(&s, &config_dir, "workstation", &state_dir);
        let snapshots = list_snapshots(&unit, &h.store).expect("list");
        let selected = select_snapshot("db", &snapshots, None)
            .expect("select")
            .clone();
        // The window the CLI leaves open while the operator answers the
        // confirmation prompt: a concurrent run's retention prune retires the
        // snapshot that was offered.
        std::fs::remove_file(&selected.path).expect("prune the payload");
        restore_backup(&unit, &h.store, &h.printer, &selected, None)
            .expect_err("a vanished snapshot must be refused, not restored from nothing")
    });

    assert!(
        matches!(
            err,
            crate::errors::CfgdError::Backup(BackupError::SnapshotMissing { .. })
        ),
        "got: {err}"
    );
    assert_eq!(
        std::fs::read_to_string(&source).expect("source"),
        "v1",
        "the target must be untouched"
    );
}

#[test]
fn an_aborted_restore_creates_none_of_the_targets_missing_parents() {
    let h = Harness::new();
    let source = h.root.join("tree");
    std::fs::create_dir_all(&source).expect("tree");
    std::fs::write(source.join("a.txt"), "v1").expect("a");
    let s = spec("db", &source);
    h.run(&s);

    let mut hooked = spec("db", &source);
    hooked.pre_backup = vec![hook("exit 4")];
    let deep = h.root.join("deep");
    let target = deep.join("nested").join("inspect");

    let outcome = h
        .restore(&hooked, None, Some(&target))
        .expect("reported, not raised");

    assert!(!outcome.restored);
    assert!(
        !deep.exists(),
        "staging must not create the target's parents before the restore commits to writing"
    );
}

#[test]
fn restore_replaces_a_directory_sitting_at_a_name_the_snapshot_holds_a_file_at() {
    let h = Harness::new();
    let source = h.root.join("tree");
    std::fs::create_dir_all(&source).expect("tree");
    std::fs::write(source.join("a.txt"), "v1").expect("a");
    let s = spec("db", &source);
    h.run(&s);

    // The kind guard only covers the top-level target; a NESTED name whose kind
    // was swapped since the snapshot has to resolve one way or the other, and
    // the snapshot's kind is the one that wins.
    std::fs::remove_file(source.join("a.txt")).expect("clear a.txt");
    std::fs::create_dir_all(source.join("a.txt").join("inner")).expect("directory in its place");
    std::fs::write(source.join("a.txt/inner/x"), "swapped").expect("inner file");

    let outcome = h.restore(&s, None, None).expect("restore");
    assert!(outcome.restored, "outcome: {outcome:?}");
    assert_eq!(
        std::fs::read_to_string(source.join("a.txt")).expect("a.txt"),
        "v1",
        "the snapshot's file must take the name back"
    );

    // The subtree it displaced is inside the target, so the safety snapshot
    // holds it — which is what makes the delete recoverable rather than lossy.
    let safety = PathBuf::from(outcome.safety_snapshot.expect("safety snapshot"));
    assert_eq!(
        std::fs::read_to_string(safety.join("a.txt/inner/x")).expect("safety payload"),
        "swapped"
    );
}

#[cfg(unix)]
#[test]
fn restore_target_reports_the_link_it_followed_alongside_what_was_asked_for() {
    let h = Harness::new();
    let real = h.root.join("real");
    std::fs::create_dir_all(&real).expect("real");
    let link = h.root.join("link");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");

    let config_dir = h.config_dir();
    let state_dir = h.state_dir();
    crate::with_test_home(&h.root, || {
        let plain = spec("db", &real);
        let unit = BackupUnit::new(&plain, &config_dir, "workstation", &state_dir);
        let target = restore_target(&unit, None);
        assert!(
            !target.was_redirected_by_a_link(),
            "an ordinary source resolves to itself: {target:?}"
        );

        let linked = spec("db", &link);
        let unit = BackupUnit::new(&linked, &config_dir, "workstation", &state_dir);
        let target = restore_target(&unit, None);
        assert!(target.was_redirected_by_a_link(), "{target:?}");
        assert_eq!(target.requested, link);
        assert_eq!(
            target.resolved,
            real.canonicalize().expect("canonical real"),
            "the resolved path is what the confirmation prompt must name"
        );
        assert_eq!(target.requested_display(), crate::to_posix_string(&link));
        assert_eq!(
            target.resolved_display(),
            crate::to_posix_string(real.canonicalize().expect("canonical real")),
            "every surface renders the path the same way"
        );
    });
}

// ---------------------------------------------------------------------------
// The `Backups` pseudo-phase inside a run
//
// `cfgd backup run`, the daemon's scheduled fire and `cfgd apply`'s pending
// backups all render through `ApplyRun`, so these assert the grammar once,
// against the skeleton all three share.
// ---------------------------------------------------------------------------

/// The icons a SETTLED item line can start with. `◐` is deliberately absent:
/// a running script's window is one live line that its outcome replaces on a
/// terminal, and counting it would double every hook.
const STATUS_ICONS: [char; 4] = ['✓', '⚠', '✗', '—'];

/// Drive `specs` through the run skeleton — header, `Backups` pseudo-phase,
/// rollup — and return the human render with its exit status.
fn render_backup_run(h: &Harness, specs: &[&BackupSpec]) -> (String, crate::state::ApplyStatus) {
    let config_dir = h.config_dir();
    let state_dir = h.state_dir();
    let (printer, buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
    let status = crate::with_test_home(&h.root, || {
        let units: Vec<BackupUnit<'_>> = specs
            .iter()
            .map(|s| BackupUnit::new(s, &config_dir, "workstation", &state_dir))
            .collect();
        let ctx = crate::reconciler::RunContext {
            title: crate::reconciler::RunTitle::Backup,
            config_path: None,
            profile: Some("workstation"),
            modules: &[],
            trigger: None,
        };
        let (status, _reports) = crate::reconciler::ApplyRun::backups(ctx, &units, &h.store)
            .execute_backups(&printer)
            .expect("a backup run renders");
        status
    });
    drop(printer);
    let human = crate::test_helpers::captured_text(&buf);
    (human, status)
}

/// The item lines the `Backups` pseudo-phase emitted: everything between its
/// heading and the rollup that carries a status icon, group headings excluded.
fn rendered_item_lines(human: &str) -> Vec<String> {
    human
        .lines()
        .skip_while(|line| line.trim() != "Backups")
        .skip(1)
        // The rollup begins at the first unindented line: every item lives
        // under an owner group and is indented, and only the phase heading
        // above (skipped) shares column 0 with the rollup. Terminating on the
        // word `action` instead read a rollup line that names no count —
        // the partial run's leading verdict — as an item of the phase.
        .take_while(|line| line.trim().is_empty() || line.starts_with(char::is_whitespace))
        .map(|line| line.trim().to_string())
        .filter(|line| line.starts_with(STATUS_ICONS))
        .collect()
}

#[test]
fn backup_hook_continue_on_error_emits_one_line() {
    let h = Harness::new();
    let source = h.seed_file("data.db", b"payload");
    let mut s = spec("db", &source);
    s.pre_backup =
        vec![serde_yaml::from_str("run: exit 5\ncontinueOnError: true\n").expect("hook")];

    let (human, _) = render_backup_run(&h, &[&s]);

    let lines = rendered_item_lines(&human);
    assert_eq!(
        lines.len(),
        2,
        "the unit's whole group is the hook's line and the snapshot's — a second summary of the same failure is one line too many: {human}"
    );
    let hook_lines: Vec<String> = lines
        .into_iter()
        .filter(|line| line.contains("preBackup:"))
        .collect();
    assert_eq!(
        hook_lines.len(),
        1,
        "a continueOnError hook failure renders ONE line, not the script's own plus a second summary: {human}"
    );
    assert!(
        hook_lines[0].starts_with('⚠'),
        "a non-fatal hook failure is a warning, not a failure: {:?}",
        hook_lines[0]
    );
    assert!(
        hook_lines[0].contains("preBackup: exit 5"),
        "the marker and the hook's own body name the line: {:?}",
        hook_lines[0]
    );

    let record = h
        .store
        .latest_backup_run("db")
        .expect("query")
        .expect("a row is written on every path");
    assert!(
        record.error.is_some(),
        "one rendered line must not cost the recorded failure: {record:?}"
    );
}

#[test]
fn backup_pre_hook_failure_reports_the_shortfall() {
    // A fatal `preBackup` failure aborts the rest of its own hook list, so the
    // second hook never renders and the run says so rather than silently
    // reporting a smaller total. The snapshot is NOT the shortfall: the copy is
    // skipped, but the record still reports it as a failed line.
    let h = Harness::new();
    let source = h.seed_file("data.db", b"payload");
    let mut s = spec("db", &source);
    s.pre_backup = vec![hook("exit 7"), hook("exit 0")];

    let (human, _) = render_backup_run(&h, &[&s]);

    let lines = rendered_item_lines(&human);
    assert_eq!(
        lines.len(),
        2,
        "the failed hook and the snapshot it cost: {human}"
    );
    assert!(
        human.contains("⊙ 1 action not attempted"),
        "the hook the abort never reached must be counted: {human}"
    );
    assert!(
        human.contains("2 actions failed"),
        "both rendered lines are failures: {human}"
    );
}

#[test]
fn backup_tally_counts_the_lines_it_rendered() {
    // §6.4's arithmetic: three succeeded items and one failed across two units
    // — a `postBackup` failure still leaves an artifact, so its snapshot line
    // counts as a success.
    let h = Harness::new();
    let clean_source = h.seed_file("data.db", b"payload");
    let warned_source = h.seed_file("secrets.env", b"secret");
    let mut clean = spec("dotfiles", &clean_source);
    clean.pre_backup = vec![hook("exit 0")];
    let mut warned = spec("secrets", &warned_source);
    warned.post_backup = vec![hook("exit 3")];

    let (human, status) = render_backup_run(&h, &[&clean, &warned]);

    let lines = rendered_item_lines(&human);
    assert_eq!(
        lines.len(),
        4,
        "two hooks and two snapshots are four lines: {human}"
    );
    assert!(
        human.contains("✓ 3 actions succeeded"),
        "the rollup counts the lines it rendered: {human}"
    );
    assert!(
        human.contains("1 action failed"),
        "the failed hook is the only failure: {human}"
    );
    assert!(
        !human.contains("not attempted"),
        "every planned item rendered, so there is no shortfall: {human}"
    );
    assert_eq!(status, crate::state::ApplyStatus::Partial);
}

#[test]
fn backup_run_header_counts_hooks_and_snapshots() {
    // The header's `Actions N planned` and the rollup's counts are two views
    // of one enumeration: one item per hook entry plus one snapshot per unit.
    let h = Harness::new();
    let first = h.seed_file("data.db", b"payload");
    let second = h.seed_file("notes.txt", b"notes");
    let mut a = spec("dotfiles", &first);
    a.pre_backup = vec![hook("exit 0")];
    a.post_backup = vec![hook("exit 0")];
    let b = spec("notes", &second);

    let (human, status) = render_backup_run(&h, &[&a, &b]);

    assert!(
        human.contains("Actions  4 planned"),
        "two hooks plus one snapshot per unit is four: {human}"
    );
    assert!(
        human.contains("✓ Backup complete — 4 actions succeeded"),
        "the rollup reconciles against the header: {human}"
    );
    assert_eq!(status, crate::state::ApplyStatus::Success);
}

#[test]
fn a_busy_unit_renders_inside_its_group_and_moves_no_exit_code() {
    let h = Harness::new();
    let source = h.seed_file("data.db", b"payload");
    let s = spec("db", &source);
    let _held = crate::acquire_backup_lock(&h.state_dir(), "db").expect("take the unit lock");

    let (human, status) = render_backup_run(&h, &[&s]);

    let lines = rendered_item_lines(&human);
    assert_eq!(lines.len(), 1, "a refused unit renders one line: {human}");
    assert!(
        lines[0].starts_with("— snapshot"),
        "a refused unit is a skip, and the group heading already named it: {:?}",
        lines[0]
    );
    assert!(
        lines[0].contains("already running (pid"),
        "the refusal names the holder: {:?}",
        lines[0]
    );
    assert!(
        human.contains("backup:db"),
        "the skip renders inside its owner group: {human}"
    );
    assert_eq!(
        status,
        crate::state::ApplyStatus::Success,
        "the one-writer rule working is not a failed run"
    );
}

#[test]
fn a_uniquified_snapshot_overhangs_the_column_it_was_predicted_for() {
    // The documented exception to `predicted_snapshot_subject`'s exactness: a
    // collision publishes `<name>-N`, which no pre-run prediction can see
    // because it depends on what is on disk when the copy runs. Dropping
    // `{timestamp}` makes the collision deterministic — it is the same one two
    // runs inside a single second produce.
    let h = Harness::new();
    let source = h.seed_file("data.db", b"payload");
    let mut s = spec("db", &source);
    s.name_pattern = "{filename}".to_string();
    let config_dir = h.config_dir();
    let state_dir = h.state_dir();
    let predicted = crate::with_test_home(&h.root, || {
        let unit = BackupUnit::new(&s, &config_dir, "workstation", &state_dir);
        predicted_snapshot_subject(&unit)
    });
    assert_eq!(predicted, "snapshot data.db");

    h.run(&s);
    let (human, _) = render_backup_run(&h, &[&s]);

    let lines = rendered_item_lines(&human);
    assert_eq!(lines.len(), 1, "one unit, one snapshot line: {human}");
    assert!(
        lines[0].contains(&format!("{predicted}-1")),
        "the collision names itself, past the column it was measured for: {:?}",
        lines[0]
    );
}

#[test]
fn a_store_failure_counts_only_the_lines_it_rendered() {
    // The one arm with no record: the snapshot landed, but the row for it
    // cannot be written. The snapshot's line is never rendered — the reporter
    // that emits it needs the record — so the failure line stands in its place
    // and the unit still contributes exactly one item per line.
    let h = Harness::new();
    let source = h.seed_file("data.db", b"payload");
    let mut s = spec("db", &source);
    s.pre_backup = vec![hook("exit 0")];
    h.store
        .drop_backup_runs_table()
        .expect("refuse the run's row");

    let (human, status) = render_backup_run(&h, &[&s]);

    let lines = rendered_item_lines(&human);
    assert_eq!(
        lines.len(),
        2,
        "the hook's line and the store failure's: {human}"
    );
    assert!(
        lines[1].starts_with("✗ snapshot"),
        "an unrecordable run has no artifact to name: {:?}",
        lines[1]
    );
    assert!(
        human.contains("✓ 1 action succeeded"),
        "only the hook succeeded — the snapshot's line was never rendered: {human}"
    );
    assert!(
        human.contains("1 action failed"),
        "the store failure is the one failure: {human}"
    );
    assert!(
        !human.contains("not attempted"),
        "both planned items reached a line, so nothing was skipped: {human}"
    );
    assert_eq!(status, crate::state::ApplyStatus::Partial);
}
