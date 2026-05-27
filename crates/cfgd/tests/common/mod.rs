//! Shared integration-test helpers.
//!
//! Each integration test file is its own crate, so any unused helper here
//! will trip `dead_code` when imported by a file that only uses some of
//! them. `#![allow(dead_code)]` is the standard Cargo idiom for this
//! shared-fixture pattern.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use cfgd::cli::{
    ApplyArgs, Cli, Command, OutputFormatArg, PlanArgs, ProfileCreateArgs, ProfileUpdateArgs,
    SourceAddArgs,
};
use cfgd_core::state::{ApplyStatus, StateStore};

/// Build a file:// URL portable across Unix and Windows.
///
/// `file_url(&path)` produces `file:///home/foo` on Unix
/// (path starts with `/`, so the result already has 3 slashes) but produces
/// `file://C:\Users\foo` on Windows — missing the third slash and using
/// backslashes — which git2 rejects with "filename, directory name, or
/// volume label syntax is incorrect".
fn file_url(path: &Path) -> String {
    let s = path.display().to_string().replace('\\', "/");
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{s}")
    }
}

/// Build a tempdir-backed profile with a single file action that will
/// succeed on apply.
///
/// Returns `(config_dir, state_dir, target)` — the tempdirs must outlive
/// the test (they own the on-disk directories) and `target` is the path
/// the action will create.
pub fn tiny_profile_setup() -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
    let config_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();

    // Source file the profile references.
    let files_dir = config_dir.path().join("files");
    std::fs::create_dir_all(&files_dir).unwrap();
    std::fs::write(files_dir.join("hello.txt"), "hello world").unwrap();

    let target = config_dir.path().join("out").join("hello.txt");
    let profile = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: tiny\nspec:\n  inherits: []\n  modules: []\n  files:\n    managed:\n      - source: files/hello.txt\n        target: {}\n        strategy: Copy\n",
        target.display()
    );
    let profiles_dir = config_dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(profiles_dir.join("tiny.yaml"), &profile).unwrap();

    let config = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: tiny\n";
    std::fs::write(config_dir.path().join("cfgd.yaml"), config).unwrap();

    (config_dir, state_dir, target)
}

/// Build a tempdir-backed profile with two file actions: one whose target
/// directory exists and is writable (succeeds), and one whose target's
/// parent is a regular file (so `create_dir_all` errors at apply time —
/// partial failure, NOT a hard error).
///
/// Returns `(config_dir, state_dir, target_ok, target_fail)`.
pub fn profile_with_one_failure_setup() -> (tempfile::TempDir, tempfile::TempDir, PathBuf, PathBuf)
{
    let config_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();

    // Both source files exist (plan stage hard-errors on missing source
    // for non-private files; we need the failure to surface at apply time).
    let files_dir = config_dir.path().join("files");
    std::fs::create_dir_all(&files_dir).unwrap();
    std::fs::write(files_dir.join("hello.txt"), "hello world").unwrap();
    std::fs::write(files_dir.join("world.txt"), "second").unwrap();

    // target_ok lands in a normal directory.
    let target_ok = config_dir.path().join("out").join("hello.txt");
    // target_fail's parent is a regular FILE on disk — `create_dir_all`
    // returns ENOTDIR at apply time. One action succeeds, one fails.
    let blocker = config_dir.path().join("blocker");
    std::fs::write(&blocker, "i am a file, not a dir").unwrap();
    let target_fail = blocker.join("world.txt");

    let profile = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: tiny\nspec:\n  inherits: []\n  modules: []\n  files:\n    managed:\n      - source: files/hello.txt\n        target: {}\n        strategy: Copy\n      - source: files/world.txt\n        target: {}\n        strategy: Copy\n",
        target_ok.display(),
        target_fail.display(),
    );
    let profiles_dir = config_dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(profiles_dir.join("tiny.yaml"), &profile).unwrap();

    let config = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: tiny\n";
    std::fs::write(config_dir.path().join("cfgd.yaml"), config).unwrap();

    (config_dir, state_dir, target_ok, target_fail)
}

/// Build a `Cli` parameterised on tempdir locations. The `command` slot is
/// filled with a no-op `Status` because the dispatcher isn't invoked —
/// integration tests call command functions directly.
pub fn cli_for(config_dir: &std::path::Path, state_dir: &std::path::Path) -> Cli {
    Cli {
        config: config_dir.join("cfgd.yaml"),
        profile: None,
        no_color: true,
        verbose: 0,
        quiet: true,
        output: OutputFormatArg(cfgd_core::output::OutputFormat::Table),
        jsonpath: None,
        state_dir: Some(state_dir.to_path_buf()),
        command: Some(Command::Status {
            module: None,
            exit_code: false,
        }),
    }
}

/// Default `ApplyArgs` for a non-dry-run `--yes` apply.
pub fn apply_args() -> ApplyArgs {
    ApplyArgs {
        from: None,
        dry_run: false,
        phase: None,
        yes: true,
        skip: vec![],
        only: vec![],
        module: None,
        skip_scripts: false,
        context: "apply".to_string(),
    }
}

/// Default `ApplyArgs` for a `--dry-run --yes` apply.
pub fn apply_args_dry_run() -> ApplyArgs {
    ApplyArgs {
        from: None,
        dry_run: true,
        phase: None,
        yes: true,
        skip: vec![],
        only: vec![],
        module: None,
        skip_scripts: false,
        context: "apply".to_string(),
    }
}

/// Build a tempdir-backed profile with zero managed files and zero modules.
/// `cmd_plan` against this fixture exercises the "nothing to do" branch.
///
/// Returns `(config_dir, state_dir)`.
pub fn empty_profile_setup() -> (tempfile::TempDir, tempfile::TempDir) {
    let config_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();

    let profile = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: empty\nspec:\n  inherits: []\n  modules: []\n";
    let profiles_dir = config_dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(profiles_dir.join("empty.yaml"), profile).unwrap();

    let config = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: empty\n";
    std::fs::write(config_dir.path().join("cfgd.yaml"), config).unwrap();

    (config_dir, state_dir)
}

/// Like `tiny_profile_setup` but pre-records an unresolved pending decision in
/// the state DB so `display_plan_preview` renders the pending-decisions
/// section.
///
/// Returns `(config_dir, state_dir, target)`.
pub fn state_with_pending_decision_setup() -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
    let (config_dir, state_dir, target) = tiny_profile_setup();

    // `cmd_plan` opens the state DB at `<state_dir>/cfgd.db` via
    // `open_state_store`; record the pending decision against the same path so
    // the subsequent `pending_decisions()` query inside `display_plan_preview`
    // sees it.
    let store = cfgd_core::state::StateStore::open(&state_dir.path().join("cfgd.db")).unwrap();
    store
        .upsert_pending_decision(
            "team-config",
            "packages.brew.ripgrep",
            "permission",
            "add",
            "team-config wants to install ripgrep",
        )
        .unwrap();

    (config_dir, state_dir, target)
}

/// Default `PlanArgs` for a plan against the active profile.
pub fn plan_args() -> PlanArgs {
    PlanArgs {
        from: None,
        phase: None,
        skip: vec![],
        only: vec![],
        module: None,
        skip_scripts: false,
        context: "apply".to_string(),
    }
}

/// `PlanArgs` with a `--module` filter set.
pub fn plan_args_module(name: &str) -> PlanArgs {
    PlanArgs {
        from: None,
        phase: None,
        skip: vec![],
        only: vec![],
        module: Some(name.to_string()),
        skip_scripts: false,
        context: "apply".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Source-sync fixtures (cmd_sync).
//
// Each fixture initialises one or more bare git repos populated with a
// `cfgd-source.yaml` manifest, then writes a cfgd config whose sources point
// at them via `file://`. `cfgd_core::sources::SourceManager` rejects
// `file://` URLs by default to prevent local-path injection; these fixtures
// require their consumer tests to set `CFGD_ALLOW_LOCAL_SOURCES=1` (handled
// per-test via `EnvVarGuard`). See `crates/cfgd-core/src/sources/mod.rs`
// for the env-var check.
// ---------------------------------------------------------------------------

fn write_manifest_to_bare(
    tmp_path: &std::path::Path,
    name: &str,
    manifest: &str,
) -> std::path::PathBuf {
    let bare = tmp_path.join(format!("{}-bare.git", name));
    let _ = git2::Repository::init_bare(&bare).expect("init_bare");

    let src = tmp_path.join(format!("{}-src", name));
    let src_repo = git2::Repository::init(&src).expect("init_src");
    std::fs::write(src.join("cfgd-source.yaml"), manifest).expect("write_manifest");
    let mut index = src_repo.index().expect("index");
    index
        .add_path(std::path::Path::new("cfgd-source.yaml"))
        .expect("add_path");
    index.write().expect("index_write");
    let tree_id = index.write_tree().expect("write_tree");
    let tree = src_repo.find_tree(tree_id).expect("find_tree");
    let sig = git2::Signature::now("t", "t@example.com").expect("signature");
    src_repo
        .commit(Some("HEAD"), &sig, &sig, "initial manifest", &tree, &[])
        .expect("commit");
    drop(tree);

    let url = file_url(&bare);
    let mut remote = src_repo.remote("origin", &url).expect("add_remote");
    let branch = src_repo
        .head()
        .expect("head")
        .shorthand()
        .unwrap_or("master")
        .to_string();
    remote
        .push(&[&format!("refs/heads/{branch}:refs/heads/{branch}")], None)
        .expect("push");
    bare
}

fn detect_branch(bare: &std::path::Path) -> String {
    let repo = git2::Repository::open(bare).unwrap();
    let refs = repo.references().unwrap();
    for r in refs.flatten() {
        if let Some(n) = r.name()
            && let Some(stripped) = n.strip_prefix("refs/heads/")
        {
            return stripped.to_string();
        }
    }
    "master".to_string()
}

const MINIMAL_MANIFEST: &str = "apiVersion: cfgd.io/v1alpha1\nkind: ConfigSource\nmetadata:\n  name: %NAME%\nspec:\n  provides:\n    profiles:\n      - default\n";

/// Two sources, both syncable from local bare repos. Returns
/// `(workspace_tmp, config_dir, state_dir)`. The workspace tmpdir owns the
/// bare repos; it must outlive the config_dir so the file:// URLs resolve.
pub fn two_source_setup() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    tempfile::TempDir,
    String,
    String,
) {
    let workspace = tempfile::tempdir().unwrap();
    let config_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();

    let bare_a = write_manifest_to_bare(
        workspace.path(),
        "team-a",
        &MINIMAL_MANIFEST.replace("%NAME%", "team-a"),
    );
    let bare_b = write_manifest_to_bare(
        workspace.path(),
        "team-b",
        &MINIMAL_MANIFEST.replace("%NAME%", "team-b"),
    );
    let branch_a = detect_branch(&bare_a);
    let branch_b = detect_branch(&bare_b);
    let url_a = file_url(&bare_a);
    let url_b = file_url(&bare_b);

    let profile = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: tiny\nspec:\n  inherits: []\n  modules: []\n";
    let profiles_dir = config_dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(profiles_dir.join("tiny.yaml"), profile).unwrap();

    let config = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: tiny\n  sources:\n    - name: team-a\n      origin:\n        type: Git\n        url: {url_a}\n        branch: {branch_a}\n    - name: team-b\n      origin:\n        type: Git\n        url: {url_b}\n        branch: {branch_b}\n"
    );
    std::fs::write(config_dir.path().join("cfgd.yaml"), config).unwrap();

    (workspace, config_dir, state_dir, branch_a, branch_b)
}

/// Config with one source whose URL points at an unreachable path. Returns
/// `(config_dir, state_dir)`. Sync's `load_source` fails for this source.
pub fn unreachable_source_setup() -> (tempfile::TempDir, tempfile::TempDir) {
    let config_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();

    let profile = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: tiny\nspec:\n  inherits: []\n  modules: []\n";
    let profiles_dir = config_dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(profiles_dir.join("tiny.yaml"), profile).unwrap();

    let config = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: tiny\n  sources:\n    - name: missing-team\n      origin:\n        type: Git\n        url: file:///nonexistent/path/that/does/not/exist.git\n        branch: master\n";
    std::fs::write(config_dir.path().join("cfgd.yaml"), config).unwrap();

    (config_dir, state_dir)
}

/// Two-stage source fixture: pre-clones a source with a permissive manifest,
/// then rewrites the bare upstream with a stricter (more "locked" items)
/// manifest so that the subsequent `cmd_sync` detects a permission change.
///
/// Returns `(workspace_tmp, config_dir, state_dir, branch)` — the workspace
/// owns the bare repo, must outlive config_dir.
pub fn permission_change_source_setup() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    tempfile::TempDir,
    String,
) {
    let workspace = tempfile::tempdir().unwrap();
    let config_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();

    // Stage 1: bare repo with the OLD permissive manifest.
    let old_manifest = "apiVersion: cfgd.io/v1alpha1\nkind: ConfigSource\nmetadata:\n  name: perm-team\nspec:\n  provides:\n    profiles:\n      - default\n  policy:\n    locked: {}\n";
    let bare = write_manifest_to_bare(workspace.path(), "perm-team", old_manifest);
    let branch = detect_branch(&bare);

    // Pre-clone the bare into state_dir/sources/perm-team so the cache dir
    // already has the OLD manifest at sync time. `cmd_sync`'s
    // `parse_manifest(old)` then sees the permissive policy.
    let cache_dir = state_dir.path().join("sources");
    std::fs::create_dir_all(&cache_dir).unwrap();
    let cached_dir = cache_dir.join("perm-team");
    let url = file_url(&bare);
    let _ = git2::build::RepoBuilder::new()
        .branch(&branch)
        .clone(&url, &cached_dir)
        .unwrap();

    // Stage 2: rewrite the bare upstream with a STRICTER manifest. Push a
    // second commit. cmd_sync's fetch picks this up; after checkout the
    // new manifest has more locked items → detect_permission_changes fires.
    let new_manifest = "apiVersion: cfgd.io/v1alpha1\nkind: ConfigSource\nmetadata:\n  name: perm-team\nspec:\n  provides:\n    profiles:\n      - default\n  policy:\n    locked:\n      env:\n        - name: TEAM_LOCK\n          value: yes\n        - name: TEAM_LOCK_2\n          value: yes\n";
    let src2 = workspace.path().join("perm-team-update");
    let src2_repo = git2::Repository::clone(&url, &src2).unwrap();
    std::fs::write(src2.join("cfgd-source.yaml"), new_manifest).unwrap();
    let mut index = src2_repo.index().unwrap();
    index
        .add_path(std::path::Path::new("cfgd-source.yaml"))
        .unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = src2_repo.find_tree(tree_id).unwrap();
    let sig = git2::Signature::now("t", "t@example.com").unwrap();
    let head = src2_repo.head().unwrap();
    let parent = head.peel_to_commit().unwrap();
    src2_repo
        .commit(
            Some("HEAD"),
            &sig,
            &sig,
            "tighten policy",
            &tree,
            &[&parent],
        )
        .unwrap();
    drop(tree);
    let mut remote = src2_repo.find_remote("origin").unwrap();
    remote
        .push(&[&format!("refs/heads/{branch}:refs/heads/{branch}")], None)
        .unwrap();

    let profile = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: tiny\nspec:\n  inherits: []\n  modules: []\n";
    let profiles_dir = config_dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(profiles_dir.join("tiny.yaml"), profile).unwrap();

    let config = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: tiny\n  sources:\n    - name: perm-team\n      origin:\n        type: Git\n        url: {url}\n        branch: {branch}\n"
    );
    std::fs::write(config_dir.path().join("cfgd.yaml"), config).unwrap();

    (workspace, config_dir, state_dir, branch)
}

// ---------------------------------------------------------------------------
// Rollback fixtures (cmd_rollback).
//
// `cmd_rollback` operates from the SQLite state DB only. Each fixture seeds
// the DB directly with applies + file backups + journal entries — no profile
// load, no reconciler.apply — so the rollback path runs against a deterministic
// state shape.
// ---------------------------------------------------------------------------

/// Seed a state DB with a target apply that has subsequent file changes to
/// roll back: apply 1 creates a file, apply 2 modifies it (capturing a
/// backup of apply 1's content). `cmd_rollback(apply_id_1)` rolls back to
/// apply 1, restoring the v1 content.
///
/// Returns `(workspace, state_dir, target_path, apply_id_1)`. The workspace
/// owns the target file; both tempdirs must outlive the test.
pub fn rollback_state_with_backups_setup() -> (tempfile::TempDir, tempfile::TempDir, PathBuf, i64) {
    let workspace = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();

    let target = workspace.path().join("config.txt");
    let file_path = target.display().to_string();

    std::fs::create_dir_all(state_dir.path()).unwrap();
    let state = StateStore::open(&state_dir.path().join("cfgd.db")).unwrap();

    // Apply 1: creates file with v1 content.
    let apply_id_1 = state
        .record_apply("test", "hash1", ApplyStatus::Success, None)
        .unwrap();
    let resource_id_1 = format!("file:create:{}", target.display());
    let jid1 = state
        .journal_begin(apply_id_1, 0, "files", "file", &resource_id_1, None)
        .unwrap();
    state.journal_complete(jid1, None, None).unwrap();
    std::fs::write(&target, "v1 content").unwrap();

    // Apply 2: backup of v1, then modify to v2.
    let file_state = cfgd_core::capture_file_state(&target).unwrap().unwrap();
    let apply_id_2 = state
        .record_apply("test", "hash2", ApplyStatus::Success, None)
        .unwrap();
    let resource_id_2 = format!("file:update:{}", target.display());
    state
        .store_file_backup(apply_id_2, &file_path, &file_state)
        .unwrap();
    let jid2 = state
        .journal_begin(apply_id_2, 0, "files", "file", &resource_id_2, None)
        .unwrap();
    state.journal_complete(jid2, None, None).unwrap();
    std::fs::write(&target, "v2 content").unwrap();

    (workspace, state_dir, target, apply_id_1)
}

/// Seed a state DB with a single apply and no subsequent changes — exercises
/// the `file_count == 0 && non_file_count == 0` branch.
pub fn rollback_state_no_changes_setup() -> (tempfile::TempDir, i64) {
    let state_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(state_dir.path()).unwrap();
    let state = StateStore::open(&state_dir.path().join("cfgd.db")).unwrap();
    let apply_id = state
        .record_apply("test", "hash1", ApplyStatus::Success, None)
        .unwrap();
    (state_dir, apply_id)
}

/// Seed a state DB with N apply rows in insertion order. `cmd_log` returns
/// them most-recent-first (via `state.history(limit)`).
///
/// Returns `(state_dir, apply_ids)` — `apply_ids[i]` is the rowid for the
/// i-th input row. `summary` defaults to `None` when the third tuple slot
/// is empty.
pub fn log_history_setup(
    rows: &[(&str, ApplyStatus, Option<&str>)],
) -> (tempfile::TempDir, Vec<i64>) {
    let state_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(state_dir.path()).unwrap();
    let state = StateStore::open(&state_dir.path().join("cfgd.db")).unwrap();

    let mut ids = Vec::with_capacity(rows.len());
    for (i, (profile, status, summary)) in rows.iter().enumerate() {
        let plan_hash = format!("hash{}", i);
        let id = state
            .record_apply(profile, &plan_hash, status.clone(), *summary)
            .unwrap();
        ids.push(id);
    }
    (state_dir, ids)
}

/// Seed a state DB with one apply and a set of journal entries. Each
/// entry's optional `script_output` is recorded via
/// `journal_complete(jid, None, script_output)`.
///
/// Returns `(state_dir, apply_id)`.
pub fn log_show_output_setup(
    entries: &[(&str, &str, &str, Option<&str>)],
) -> (tempfile::TempDir, i64) {
    let state_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(state_dir.path()).unwrap();
    let state = StateStore::open(&state_dir.path().join("cfgd.db")).unwrap();

    let apply_id = state
        .record_apply("test", "hash1", ApplyStatus::Success, None)
        .unwrap();
    for (idx, (phase, action_type, resource_id, script_output)) in entries.iter().enumerate() {
        let jid = state
            .journal_begin(apply_id, idx, phase, action_type, resource_id, None)
            .unwrap();
        state.journal_complete(jid, None, *script_output).unwrap();
    }
    (state_dir, apply_id)
}

/// Seed a state DB with one apply recorded via `record_apply` but **no**
/// journal entries (`journal_begin` is never called). Exercises the
/// `cmd_log_show_output` branch where `state.journal_entries(apply_id)`
/// returns an empty Vec.
///
/// Returns `(state_dir, apply_id)`.
pub fn log_show_output_no_journal_setup() -> (tempfile::TempDir, i64) {
    let state_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(state_dir.path()).unwrap();
    let state = StateStore::open(&state_dir.path().join("cfgd.db")).unwrap();

    let apply_id = state
        .record_apply("test", "hash1", ApplyStatus::Success, None)
        .unwrap();
    (state_dir, apply_id)
}

// ---------------------------------------------------------------------------
// Profile fixtures (cmd_profile_*).
//
// Each fixture writes a `cfgd.yaml` + one or more `profiles/<name>.yaml` files
// into a tempdir so the profile commands can exercise their on-disk paths.
// ---------------------------------------------------------------------------

const PROFILE_DEFAULT_YAML: &str = r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: default
spec:
  env:
    - name: EDITOR
      value: vim
"#;

const PROFILE_WORK_YAML: &str = r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: work
spec:
  inherits:
    - default
  env:
    - name: EDITOR
      value: code
"#;

const PROFILE_CFGD_CONFIG_YAML: &str =
    "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n";

/// Tempdir-backed `cfgd.yaml` + `profiles/default.yaml` + `profiles/work.yaml`.
/// Returns `(config_dir, state_dir)` — both tempdirs must outlive the test.
/// `work` inherits from `default` so the inheritor-refusal path is reachable.
pub fn profile_test_config_setup() -> (tempfile::TempDir, tempfile::TempDir) {
    let config_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let profiles_dir = config_dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        config_dir.path().join("cfgd.yaml"),
        PROFILE_CFGD_CONFIG_YAML,
    )
    .unwrap();
    std::fs::write(profiles_dir.join("default.yaml"), PROFILE_DEFAULT_YAML).unwrap();
    std::fs::write(profiles_dir.join("work.yaml"), PROFILE_WORK_YAML).unwrap();
    (config_dir, state_dir)
}

/// Like `profile_test_config_setup` but writes ONLY `default.yaml` (no `work`),
/// so the inheritor-refusal path isn't reachable. Use when the test needs a
/// minimal single-profile dir.
pub fn profile_test_config_single_setup() -> (tempfile::TempDir, tempfile::TempDir) {
    let config_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let profiles_dir = config_dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        config_dir.path().join("cfgd.yaml"),
        PROFILE_CFGD_CONFIG_YAML,
    )
    .unwrap();
    std::fs::write(profiles_dir.join("default.yaml"), PROFILE_DEFAULT_YAML).unwrap();
    (config_dir, state_dir)
}

/// Default `ProfileCreateArgs` — empty flags so the command body takes the
/// interactive path (used by snapshot tests that drive `prompt_text`).
pub fn profile_create_args(name: &str) -> ProfileCreateArgs {
    ProfileCreateArgs {
        name: name.to_string(),
        inherits: vec![],
        modules: vec![],
        packages: vec![],
        env: vec![],
        aliases: vec![],
        system: vec![],
        files: vec![],
        private: false,
        secrets: vec![],
        pre_apply: vec![],
        post_apply: vec![],
        pre_reconcile: vec![],
        post_reconcile: vec![],
        on_change: vec![],
        on_drift: vec![],
    }
}

/// Default `ProfileUpdateArgs` — empty add/remove vectors. Mutate the returned
/// struct directly to add specific args (e.g. `args.modules = vec![url]`).
pub fn profile_update_args() -> ProfileUpdateArgs {
    ProfileUpdateArgs {
        name: None,
        inherits: vec![],
        modules: vec![],
        packages: vec![],
        files: vec![],
        env: vec![],
        aliases: vec![],
        system: vec![],
        secrets: vec![],
        pre_apply: vec![],
        post_apply: vec![],
        pre_reconcile: vec![],
        post_reconcile: vec![],
        on_change: vec![],
        on_drift: vec![],
        private: false,
    }
}

/// Initialise a bare upstream + a working source repo, commit a minimal
/// `module.yaml` at the root, annotate it with `tag`, and push both the
/// branch and the tag to the bare. Returns the bare path so
/// `file://<bare>@<tag>` can be used as a remote module URL.
///
/// Mirrors `cmd_module_add_remote_local_bare::make_bare_with_module` in
/// `crates/cfgd/src/cli/module/tests.rs`; lifted here so integration test
/// crates can drive `cmd_profile_update --module <file://...>` against a
/// hermetic remote.
pub fn make_bare_module_repo(
    tmp_root: &std::path::Path,
    module_name: &str,
    tag: &str,
) -> std::path::PathBuf {
    let bare = tmp_root.join(format!("{}-upstream.git", module_name));
    let _bare_repo = git2::Repository::init_bare(&bare).expect("init_bare");

    let src = tmp_root.join(format!("{}-src", module_name));
    let src_repo = git2::Repository::init(&src).expect("init_src");
    let yaml = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: {}\n  description: test mod\nspec: {{}}\n",
        module_name
    );
    std::fs::write(src.join("module.yaml"), yaml).expect("write module.yaml");
    let mut index = src_repo.index().expect("index");
    index
        .add_path(std::path::Path::new("module.yaml"))
        .expect("add_path");
    index.write().expect("index_write");
    let tree_id = index.write_tree().expect("write_tree");
    let tree = src_repo.find_tree(tree_id).expect("find_tree");
    let sig = git2::Signature::now("t", "t@example.com").expect("signature");
    let commit_id = src_repo
        .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
        .expect("commit");
    drop(tree);
    let commit_obj = src_repo.find_commit(commit_id).expect("find_commit");
    src_repo
        .tag(tag, commit_obj.as_object(), &sig, "release", false)
        .expect("tag");

    let bare_url = file_url(&bare);
    let mut remote = src_repo.remote("origin", &bare_url).expect("add_remote");
    let branch = src_repo
        .head()
        .expect("head")
        .shorthand()
        .unwrap_or("master")
        .to_string();
    remote
        .push(
            &[
                &format!("refs/heads/{branch}:refs/heads/{branch}"),
                &format!("refs/tags/{tag}:refs/tags/{tag}"),
            ],
            None,
        )
        .expect("push");
    bare
}

/// Normalize tempdir-rooted paths in a captured snapshot to stable placeholders
/// so goldens are host-stable across runs. Folds `\` → `/` so windows-native
/// path separators in captured output match POSIX fixtures.
pub fn normalize_profile_paths(raw: &str, config_dir: &std::path::Path) -> String {
    let mut out = raw.to_string();
    let cfg_file = config_dir.join("cfgd.yaml");
    out = out.replace(
        &cfg_file.to_string_lossy().to_string(),
        "<CONFIG_DIR>/cfgd.yaml",
    );
    out = out.replace(&config_dir.to_string_lossy().to_string(), "<CONFIG_DIR>");
    out.replace('\\', "/")
}

// ---------------------------------------------------------------------------
// Source fixtures (cmd_source_*).
//
// Tempdir-backed `cfgd.yaml` (+ optional sources entry); bare git repos for
// `cmd_source_add` happy paths. Bare-source fixtures pair with the
// `CFGD_ALLOW_LOCAL_SOURCES=1` env-var guard so `file://` URLs are accepted.
// ---------------------------------------------------------------------------

const SOURCE_CFGD_CONFIG_YAML: &str =
    "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n";

const SOURCE_DEFAULT_PROFILE_YAML: &str =
    "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n";

/// Bare-bones config dir for source-command tests: writes `cfgd.yaml` +
/// `profiles/default.yaml` so command bodies that load the active profile
/// find one. Returns `(config_dir, state_dir)` — both tempdirs must outlive
/// the test.
pub fn source_test_config_setup() -> (tempfile::TempDir, tempfile::TempDir) {
    let config_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let profiles_dir = config_dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(config_dir.path().join("cfgd.yaml"), SOURCE_CFGD_CONFIG_YAML).unwrap();
    std::fs::write(
        profiles_dir.join("default.yaml"),
        SOURCE_DEFAULT_PROFILE_YAML,
    )
    .unwrap();
    (config_dir, state_dir)
}

/// `source_test_config_setup` plus one source entry in `cfgd.yaml` referencing
/// the supplied URL and branch. Use when the test needs to exercise an
/// already-subscribed source (`cmd_source_remove`, `cmd_source_update`,
/// `cmd_source_show`, `cmd_source_priority`, `cmd_source_override`).
pub fn source_test_config_with_source_setup(
    source_name: &str,
    url: &str,
    branch: &str,
    priority: u32,
) -> (tempfile::TempDir, tempfile::TempDir) {
    let config_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let profiles_dir = config_dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("default.yaml"),
        SOURCE_DEFAULT_PROFILE_YAML,
    )
    .unwrap();
    let config = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n  sources:\n    - name: {source_name}\n      origin:\n        type: Git\n        url: {url}\n        branch: {branch}\n      subscription:\n        priority: {priority}\n"
    );
    std::fs::write(config_dir.path().join("cfgd.yaml"), config).unwrap();
    (config_dir, state_dir)
}

/// Default `SourceAddArgs` carrying the supplied URL; everything else
/// defaults so each test mutates only the slots it cares about.
pub fn source_add_args(url: impl Into<String>) -> SourceAddArgs {
    SourceAddArgs {
        url: url.into(),
        name: None,
        branch: None,
        profile: None,
        accept_recommended: false,
        priority: None,
        opt_in: vec![],
        sync_interval: None,
        auto_apply: false,
        pin_version: None,
        yes: true,
    }
}

/// Initialise a bare upstream + a working source repo, commit a minimal
/// `cfgd-source.yaml` at the root, push to the bare. Returns the bare path
/// so `file://<bare>` can be used as a remote source URL.
///
/// Mirrors `make_bare_module_repo` shape, adapted to the source manifest.
pub fn make_bare_source_repo(
    tmp_root: &std::path::Path,
    source_name: &str,
    extra_spec: Option<&str>,
) -> std::path::PathBuf {
    let bare = tmp_root.join(format!("{}-source.git", source_name));
    let _bare_repo = git2::Repository::init_bare(&bare).expect("init_bare");

    let src = tmp_root.join(format!("{}-src", source_name));
    let src_repo = git2::Repository::init(&src).expect("init_src");
    let extra = extra_spec.unwrap_or("");
    let yaml = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: ConfigSource\nmetadata:\n  name: {source_name}\n  version: \"1.0.0\"\nspec:\n  provides:\n    profiles:\n      - default\n{extra}",
    );
    std::fs::write(src.join("cfgd-source.yaml"), yaml).expect("write cfgd-source.yaml");
    let mut index = src_repo.index().expect("index");
    index
        .add_path(std::path::Path::new("cfgd-source.yaml"))
        .expect("add_path");
    index.write().expect("index_write");
    let tree_id = index.write_tree().expect("write_tree");
    let tree = src_repo.find_tree(tree_id).expect("find_tree");
    let sig = git2::Signature::now("t", "t@example.com").expect("signature");
    src_repo
        .commit(Some("HEAD"), &sig, &sig, "initial manifest", &tree, &[])
        .expect("commit");
    drop(tree);

    let bare_url = file_url(&bare);
    let mut remote = src_repo.remote("origin", &bare_url).expect("add_remote");
    let branch = src_repo
        .head()
        .expect("head")
        .shorthand()
        .unwrap_or("master")
        .to_string();
    remote
        .push(&[&format!("refs/heads/{branch}:refs/heads/{branch}")], None)
        .expect("push");
    bare
}

/// Clone `bare` into a fresh workdir, replace its `cfgd-source.yaml` with
/// `new_manifest_yaml`, commit + push back to the bare. Mirrors the
/// `push_replacement_manifest` helper used by the permission-change unit
/// tests in `cli/tests.rs`. Use to seed a "v2" manifest atop a bare so
/// `cmd_source_update` exercises the permission-expansion prompt path.
pub fn push_replacement_manifest_to_bare(
    scratch: &std::path::Path,
    bare: &std::path::Path,
    new_manifest_yaml: &str,
) {
    let clone_dir = scratch.join(format!(
        "replace-clone-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let url = file_url(&bare);
    let repo = git2::Repository::clone(&url, &clone_dir).unwrap();
    std::fs::write(clone_dir.join("cfgd-source.yaml"), new_manifest_yaml).unwrap();
    let mut index = repo.index().unwrap();
    index
        .add_path(std::path::Path::new("cfgd-source.yaml"))
        .unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let sig = git2::Signature::now("t", "t@example.com").unwrap();
    let parent = repo.head().unwrap().peel_to_commit().unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "v2 manifest", &tree, &[&parent])
        .unwrap();
    drop(tree);
    let branch = repo
        .head()
        .unwrap()
        .shorthand()
        .unwrap_or("master")
        .to_string();
    let mut remote = repo.find_remote("origin").unwrap();
    remote
        .push(
            &[&format!("+refs/heads/{branch}:refs/heads/{branch}")],
            None,
        )
        .unwrap();
}

/// Seed a state DB with a target apply followed by a non-file (package)
/// action — exercises the "Non-file actions (manual review)" section.
pub fn rollback_state_with_non_file_actions_setup() -> (tempfile::TempDir, i64) {
    let state_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(state_dir.path()).unwrap();
    let state = StateStore::open(&state_dir.path().join("cfgd.db")).unwrap();

    let apply_id_1 = state
        .record_apply("test", "hash1", ApplyStatus::Success, None)
        .unwrap();

    let apply_id_2 = state
        .record_apply("test", "hash2", ApplyStatus::Success, None)
        .unwrap();
    let jid = state
        .journal_begin(
            apply_id_2,
            0,
            "packages",
            "package",
            "package:brew:install:ripgrep",
            None,
        )
        .unwrap();
    state.journal_complete(jid, None, None).unwrap();

    (state_dir, apply_id_1)
}

/// Pre-stage a minimal `cfgd.yaml` so `cmd_config_*` finds a parseable
/// config + a `spec` section. The default profile field lets get/set/unset
/// exercise existing-key paths.
pub fn config_test_setup() -> (tempfile::TempDir, tempfile::TempDir) {
    let config_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(config_dir.path().join("profiles")).unwrap();
    std::fs::write(
        config_dir.path().join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n  theme:\n    name: monokai\n",
    )
    .unwrap();
    std::fs::write(
        config_dir.path().join("profiles/default.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
    )
    .unwrap();
    (config_dir, state_dir)
}

/// Same shape as `config_test_setup` but without a `cfgd.yaml` — for
/// no-config error-path cases.
pub fn config_test_setup_no_config() -> (tempfile::TempDir, tempfile::TempDir) {
    let config_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(config_dir.path().join("profiles")).unwrap();
    (config_dir, state_dir)
}

/// Pre-stage a config_dir for `cfgd secret *` commands. The minimal
/// `cfgd.yaml` declares the sops backend (default), which `get_secret_backend`
/// reads to dispatch encryption.
pub fn secret_test_setup() -> (tempfile::TempDir, tempfile::TempDir) {
    let config_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(config_dir.path().join("profiles")).unwrap();
    std::fs::write(
        config_dir.path().join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n  secrets:\n    backend: sops\n",
    )
    .unwrap();
    std::fs::write(
        config_dir.path().join("profiles/default.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
    )
    .unwrap();
    (config_dir, state_dir)
}

/// Pre-stage a config_dir for `cfgd workflow generate`. One profile + one
/// module so the generator finds targets and writes a non-placeholder YAML.
pub fn workflow_test_setup() -> (tempfile::TempDir, tempfile::TempDir) {
    let config_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(config_dir.path().join("profiles")).unwrap();
    std::fs::create_dir_all(config_dir.path().join("modules").join("neovim")).unwrap();
    std::fs::write(
        config_dir.path().join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n",
    )
    .unwrap();
    std::fs::write(
        config_dir.path().join("profiles/default.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
    )
    .unwrap();
    std::fs::write(
        config_dir.path().join("modules/neovim/module.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: neovim\nspec:\n  version: 0.1.0\n",
    )
    .unwrap();
    (config_dir, state_dir)
}

/// Like `workflow_test_setup` but with no profiles or modules — the
/// `no_profiles` warning branch.
pub fn workflow_empty_test_setup() -> (tempfile::TempDir, tempfile::TempDir) {
    let config_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(config_dir.path().join("profiles")).unwrap();
    std::fs::write(
        config_dir.path().join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n",
    )
    .unwrap();
    (config_dir, state_dir)
}
