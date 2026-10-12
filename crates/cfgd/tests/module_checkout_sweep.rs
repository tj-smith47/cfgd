//! A locked module's files link into the cached tree of the commit it is
//! locked to. `cfgd module upgrade` moves the lock to a new commit and leaves
//! the old tree alone, because the links deployed from it still resolve there
//! until the next apply replaces them. That apply then removes every tree no
//! lock entry and no deployed link reads any more.
//!
//! The verbs run as the real binary, because `apply` ends in
//! `std::process::exit`.

use std::path::{Path, PathBuf};

mod cfgd_binary;
use cfgd_binary::cfgd_bin;

fn cfgd_at(config_dir: &Path, state_dir: &Path, args: &[&str]) -> std::process::Output {
    cfgd_bin()
        .unwrap()
        .arg("--config")
        .arg(config_dir.join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_dir)
        .args(["--migration-policy", "ignore"])
        .env(cfgd_core::CFGD_ALLOW_LOCAL_SOURCES_ENV, "1")
        .env(cfgd_core::CFGD_CACHE_DIR_ENV, state_dir.join("cache"))
        .args(args)
        .output()
        .unwrap()
}

fn assert_runs(config_dir: &Path, state_dir: &Path, args: &[&str]) {
    let out = cfgd_at(config_dir, state_dir, args);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{args:?}: stdout {} stderr {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repository holding two modules, `tools` and `extra`, each deploying one
/// file by symlink to a target under `targets`. Every `(tag, text)` adds a
/// commit writing `text` to both modules' files and tags it. Returns the
/// repository's temp dir and its `file://` URL.
fn two_module_repo(targets: &Path, tags: &[(&str, &str)]) -> (tempfile::TempDir, String) {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(repo_dir.path()).unwrap();
    let sig = git2::Signature::now("t", "t@example.com").unwrap();
    // Integrity is a digest of the checked-out bytes, so no checkout may
    // rewrite their line endings.
    std::fs::write(repo_dir.path().join(".gitattributes"), "* -text\n").unwrap();
    for module in ["tools", "extra"] {
        let dir = repo_dir.path().join(module);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("module.yaml"),
            format!(
                "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: {module}\nspec:\n  files:\n    - source: rc\n      target: {}\n      strategy: Symlink\n",
                cfgd_core::to_posix_string(targets.join(module))
            ),
        )
        .unwrap();
    }
    let mut parent: Option<git2::Oid> = None;
    for (tag, text) in tags {
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(".gitattributes")).unwrap();
        for module in ["tools", "extra"] {
            std::fs::write(repo_dir.path().join(module).join("rc"), text).unwrap();
            index
                .add_path(&Path::new(module).join("module.yaml"))
                .unwrap();
            index.add_path(&Path::new(module).join("rc")).unwrap();
        }
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let parents: Vec<git2::Commit> = parent
            .iter()
            .map(|id| repo.find_commit(*id).unwrap())
            .collect();
        let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
        let id = repo
            .commit(Some("HEAD"), &sig, &sig, tag, &tree, &parent_refs)
            .unwrap();
        repo.tag_lightweight(tag, repo.find_commit(id).unwrap().as_object(), false)
            .unwrap();
        parent = Some(id);
    }
    let url = cfgd_core::to_file_url(repo_dir.path());
    (repo_dir, url)
}

/// The cached tree `requested_ref` of `url` resolves to, for the runs
/// `cfgd_at` starts.
fn checkout(state_dir: &Path, url: &str, requested_ref: &str) -> PathBuf {
    let cache_base =
        cfgd_core::module_cache_root(Some(&state_dir.join("cache")), cfgd_core::Scope::User)
            .unwrap();
    cfgd_core::modules::git_checkout_dir(&cache_base, url, Some(requested_ref)).unwrap()
}

/// Move the modification time of the tree at `tree`, and of the in-use stamp
/// cfgd keeps for it beside the clone's other bookkeeping, two hours back.
fn backdate(tree: &Path) {
    let then = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 60 * 60);
    cfgd_core::test_helpers::set_dir_modified(tree, then);
    let stamp = tree
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join("cfgd-in-use")
        .join(tree.file_name().unwrap());
    std::fs::File::options()
        .write(true)
        .open(&stamp)
        .unwrap_or_else(|e| panic!("{}: {e}", stamp.display()))
        .set_modified(then)
        .unwrap();
}

fn read_through(link: &Path) -> String {
    let text = std::fs::read_to_string(link)
        .unwrap_or_else(|e| panic!("{} does not resolve: {e}", cfgd_core::to_posix_string(link)));
    cfgd_core::normalize_line_endings(&text).into_owned()
}

#[test]
fn an_upgrade_keeps_the_old_tree_until_the_next_apply_and_that_apply_removes_it() {
    let config_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let targets = config_dir.path().join("out");
    let (_repo, url) = two_module_repo(&targets, &[("v1", "one\n"), ("v2", "two\n")]);
    std::fs::create_dir_all(config_dir.path().join("profiles")).unwrap();
    std::fs::write(
        config_dir.path().join("profiles").join("tiny.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: tiny\nspec:\n  modules: []\n",
    )
    .unwrap();
    std::fs::write(
        config_dir.path().join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: tiny\n",
    )
    .unwrap();
    let (config, state) = (config_dir.path(), state_dir.path());
    for module in ["tools", "extra"] {
        let source = format!("{url}//{module}@v1");
        assert_runs(
            config,
            state,
            &[
                "--yes",
                "profile",
                "update",
                "tiny",
                "--module",
                &source,
                "--allow-unsigned",
            ],
        );
    }
    assert_runs(config, state, &["--yes", "apply"]);
    let (tools, extra) = (targets.join("tools"), targets.join("extra"));
    assert_eq!(read_through(&tools), "one\n");
    assert!(
        tools.symlink_metadata().unwrap().is_symlink(),
        "the module deploys its file as a link into the cached tree"
    );
    let v1 = checkout(state, &url, "v1");
    let v2 = checkout(state, &url, "v2");

    assert_runs(
        config,
        state,
        &[
            "--yes",
            "module",
            "upgrade",
            "tools",
            "--ref",
            "v2",
            "--allow-unsigned",
        ],
    );
    assert_eq!(
        read_through(&tools),
        "one\n",
        "the link deployed before the upgrade resolves until an apply replaces it"
    );

    assert_runs(config, state, &["--yes", "apply"]);
    assert_eq!(
        read_through(&tools),
        "two\n",
        "the apply relinked the upgraded module"
    );
    assert_eq!(read_through(&extra), "one\n");
    assert!(
        v1.is_dir(),
        "the tree another lock entry still reads stays after the apply"
    );

    assert_runs(
        config,
        state,
        &[
            "--yes",
            "module",
            "upgrade",
            "extra",
            "--ref",
            "v2",
            "--allow-unsigned",
        ],
    );
    // An apply leaves a tree handed out within the last hour alone, and the
    // upgrade above handed v1 out again when it resolved the old lock.
    backdate(&v1);
    assert_runs(config, state, &["--yes", "apply"]);
    assert_eq!(read_through(&extra), "two\n");
    assert!(
        !v1.exists(),
        "the apply removed the tree nothing reads any more"
    );
    assert!(v2.is_dir());
}
