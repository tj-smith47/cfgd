//! Shared integration-test helpers.
//!
//! Each integration test file is its own crate, so any unused helper here
//! will trip `dead_code` when imported by a file that only uses some of
//! them. `#![allow(dead_code)]` is the standard Cargo idiom for this
//! shared-fixture pattern.

#![allow(dead_code)]

use std::path::PathBuf;

use cfgd::cli::{ApplyArgs, Cli, Command, OutputFormatArg, PlanArgs};
use cfgd_core::output;

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
        output: OutputFormatArg(output::OutputFormat::Table),
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
/// the state DB so `display_plan_preview_v2` renders the pending-decisions
/// section.
///
/// Returns `(config_dir, state_dir, target)`.
pub fn state_with_pending_decision_setup() -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
    let (config_dir, state_dir, target) = tiny_profile_setup();

    // `cmd_plan` opens the state DB at `<state_dir>/cfgd.db` via
    // `open_state_store`; record the pending decision against the same path so
    // the subsequent `pending_decisions()` query inside `display_plan_preview_v2`
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
// Source-sync fixtures (T3 / cmd_sync)
//
// Each fixture initialises one or more bare git repos populated with a
// `cfgd-source.yaml` manifest, then writes a cfgd config whose sources point
// at them via `file://`. `CFGD_ALLOW_LOCAL_SOURCES=1` must be set in the env
// that drives `cmd_sync` (handled per-test via `EnvVarGuard`) because the
// SourceManager rejects file:// URLs otherwise.
// ---------------------------------------------------------------------------

fn write_manifest_to_bare(
    tmp_path: &std::path::Path,
    name: &str,
    manifest: &str,
) -> std::path::PathBuf {
    let bare = tmp_path.join(format!("{}-bare.git", name));
    let _ = git2::Repository::init_bare(&bare).unwrap();

    let src = tmp_path.join(format!("{}-src", name));
    let src_repo = git2::Repository::init(&src).unwrap();
    std::fs::write(src.join("cfgd-source.yaml"), manifest).unwrap();
    let mut index = src_repo.index().unwrap();
    index
        .add_path(std::path::Path::new("cfgd-source.yaml"))
        .unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = src_repo.find_tree(tree_id).unwrap();
    let sig = git2::Signature::now("t", "t@example.com").unwrap();
    src_repo
        .commit(Some("HEAD"), &sig, &sig, "initial manifest", &tree, &[])
        .unwrap();
    drop(tree);

    let url = format!("file://{}", bare.display());
    let mut remote = src_repo.remote("origin", &url).unwrap();
    let branch = src_repo
        .head()
        .unwrap()
        .shorthand()
        .unwrap_or("master")
        .to_string();
    remote
        .push(&[&format!("refs/heads/{branch}:refs/heads/{branch}")], None)
        .unwrap();
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
    let url_a = format!("file://{}", bare_a.display());
    let url_b = format!("file://{}", bare_b.display());

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
    let url = format!("file://{}", bare.display());
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
