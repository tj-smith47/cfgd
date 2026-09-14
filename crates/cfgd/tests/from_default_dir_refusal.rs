#![allow(deprecated)] // assert_cmd 2.x cargo_bin deprecation; upgrade path is assert_cmd 3.x

//! A `--from` run that names no destination must not write over the default
//! config directory.
//!
//! An e2e suite pointed `apply --from <scratch repo>` at a scratch `--config`
//! and the run cloned the fixture into the invoking user's own
//! `~/.config/cfgd` instead, because a `--config` that already existed was read
//! as "no destination given". These run the real binary: the refusal is an exit
//! code and a message, and both are what a caller in a `&&` chain sees.

use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;

/// A committed git repository holding a `cfgd.yaml`, for `--from` to clone.
fn source_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: from-src\nspec:\n  profile: base\n",
    )
    .unwrap();
    let repo = git2::Repository::init(dir).unwrap();
    let sig = git2::Signature::now("Test", "test@example.com").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("cfgd.yaml")).unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .unwrap();
}

/// The binary under a throwaway home, with every seam that resolves the default
/// config directory pointed into it.
fn run(home: &Path, args: &[&str]) -> assert_cmd::assert::Assert {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(args)
        .env("HOME", home)
        // Windows resolves `~` from USERPROFILE first.
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("CFGD_CACHE_DIR", home.join("cache"))
        .env("CFGD_ALLOW_LOCAL_SOURCES", "1")
        .assert()
}

/// The default config directory under a throwaway home, created empty.
fn default_config_dir(home: &Path) -> std::path::PathBuf {
    let dir = home.join(".config").join("cfgd");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn init_from_refuses_a_default_dir_that_already_holds_a_config() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    source_repo(&src);
    let home = tmp.path().join("home");
    let dest = default_config_dir(&home);
    std::fs::write(dest.join("cfgd.yaml"), "apiVersion: cfgd.io/v1alpha1\n").unwrap();

    run(&home, &["init", "--from", &src.display().to_string()])
        .code(1)
        .stderr(predicate::str::contains(
            "Refusing to write into the default config directory",
        ))
        .stderr(predicate::str::contains("it already holds a cfgd.yaml"));

    // The refusal is only worth anything if it left the directory alone.
    assert_eq!(
        std::fs::read_to_string(dest.join("cfgd.yaml")).unwrap(),
        "apiVersion: cfgd.io/v1alpha1\n"
    );
    assert!(!dest.join(".git").exists(), "nothing was cloned over it");
}

#[test]
fn init_from_refuses_a_non_empty_default_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    source_repo(&src);
    let home = tmp.path().join("home");
    let dest = default_config_dir(&home);
    std::fs::create_dir_all(dest.join(".git")).unwrap();
    std::fs::write(dest.join("notes.txt"), "somebody's").unwrap();

    run(&home, &["init", "--from", &src.display().to_string()])
        .code(1)
        .stderr(predicate::str::contains("it is not empty"));

    assert_eq!(
        std::fs::read_to_string(dest.join("notes.txt")).unwrap(),
        "somebody's"
    );
}

#[cfg(unix)]
#[test]
fn init_from_refuses_a_symlinked_default_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    source_repo(&src);
    let home = tmp.path().join("home");
    let real = tmp.path().join("elsewhere");
    std::fs::create_dir_all(&real).unwrap();
    std::fs::create_dir_all(home.join(".config")).unwrap();
    std::os::unix::fs::symlink(&real, home.join(".config").join("cfgd")).unwrap();

    run(&home, &["init", "--from", &src.display().to_string()])
        .code(1)
        .stderr(predicate::str::contains("it is a symlink"));

    assert!(
        std::fs::read_dir(&real).unwrap().next().is_none(),
        "the directory the link points at was left alone"
    );
}

#[test]
fn apply_from_refuses_a_default_dir_that_already_holds_a_config() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    source_repo(&src);
    let home = tmp.path().join("home");
    let dest = default_config_dir(&home);
    std::fs::write(dest.join("cfgd.yaml"), "apiVersion: cfgd.io/v1alpha1\n").unwrap();

    // `--config` at the default file is what a plain `cfgd apply --from` sees,
    // and the found config used to be applied against the real machine.
    run(
        &home,
        &[
            "apply",
            "--dry-run",
            "--yes",
            "--from",
            &src.display().to_string(),
        ],
    )
    .code(1)
    .stderr(predicate::str::contains(
        "Refusing to write into the default config directory",
    ));
}

#[test]
fn init_from_still_clones_into_an_unoccupied_default_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    source_repo(&src);
    let home = tmp.path().join("home");
    let dest = default_config_dir(&home);

    run(&home, &["init", "--from", &src.display().to_string()]).success();

    let cloned = std::fs::read_to_string(dest.join("cfgd.yaml")).unwrap();
    assert!(cloned.contains("name: from-src"), "got: {cloned}");
}

#[test]
fn apply_from_materialises_into_the_directory_config_names() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    source_repo(&src);
    let home = tmp.path().join("home");
    let dest = default_config_dir(&home);
    std::fs::write(dest.join("cfgd.yaml"), "apiVersion: cfgd.io/v1alpha1\n").unwrap();

    // The destination `--config` names already holds a config file — the arm
    // that used to fall back to the default directory instead.
    let named = tmp.path().join("named");
    std::fs::create_dir_all(&named).unwrap();
    std::fs::write(
        named.join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: named\nspec:\n  profile: base\n",
    )
    .unwrap();
    std::fs::create_dir_all(named.join("profiles")).unwrap();
    std::fs::write(
        named.join("profiles").join("base.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: base\nspec:\n  inherits: []\n  modules: []\n",
    )
    .unwrap();

    run(
        &home,
        &[
            "apply",
            "--dry-run",
            "--yes",
            "--from",
            &src.display().to_string(),
            "--config",
            &named.join("cfgd.yaml").display().to_string(),
            "--state-dir",
            &tmp.path().join("state").display().to_string(),
        ],
    )
    .success();

    // The default directory kept the config it already had.
    assert_eq!(
        std::fs::read_to_string(dest.join("cfgd.yaml")).unwrap(),
        "apiVersion: cfgd.io/v1alpha1\n"
    );
    assert!(!dest.join(".git").exists());
}

#[test]
fn plan_from_refuses_a_default_dir_that_already_holds_a_config() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    source_repo(&src);
    let home = tmp.path().join("home");
    let dest = default_config_dir(&home);
    std::fs::write(dest.join("cfgd.yaml"), "apiVersion: cfgd.io/v1alpha1\n").unwrap();

    // `plan` carries its own copy of the two lines `apply` has, and nothing
    // pinned it: the refusal is documented as covering all three verbs.
    run(&home, &["plan", "--from", &src.display().to_string()])
        .code(1)
        .stderr(predicate::str::contains(
            "Refusing to write into the default config directory",
        ));

    assert_eq!(
        std::fs::read_to_string(dest.join("cfgd.yaml")).unwrap(),
        "apiVersion: cfgd.io/v1alpha1\n"
    );
}

#[test]
fn apply_from_refuses_a_config_that_walks_back_into_the_default_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    source_repo(&src);
    let home = tmp.path().join("home");
    let dest = default_config_dir(&home);
    std::fs::write(dest.join("cfgd.yaml"), "apiVersion: cfgd.io/v1alpha1\n").unwrap();

    // `absolutize_path` leaves `..` a literal component, so this spelling names
    // the default directory under a string no comparison with it matches.
    let walked = home
        .join(".config")
        .join("cfgd")
        .join("..")
        .join("cfgd")
        .join("cfgd.yaml");
    run(
        &home,
        &[
            "apply",
            "--dry-run",
            "--yes",
            "--from",
            &src.display().to_string(),
            "--config",
            &walked.display().to_string(),
        ],
    )
    .code(1)
    .stderr(predicate::str::contains(
        "Refusing to write into the default config directory",
    ));

    assert_eq!(
        std::fs::read_to_string(dest.join("cfgd.yaml")).unwrap(),
        "apiVersion: cfgd.io/v1alpha1\n"
    );
    assert!(!dest.join(".git").exists(), "nothing was cloned over it");
}

#[cfg(unix)]
#[test]
fn apply_from_refuses_a_config_pointed_at_what_the_default_dir_links_to() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    source_repo(&src);
    let home = tmp.path().join("home");
    let real = tmp.path().join("elsewhere");
    std::fs::create_dir_all(&real).unwrap();
    std::fs::write(real.join("cfgd.yaml"), "apiVersion: cfgd.io/v1alpha1\n").unwrap();
    std::fs::create_dir_all(home.join(".config")).unwrap();
    std::os::unix::fs::symlink(&real, home.join(".config").join("cfgd")).unwrap();

    // The link's target is the same directory under a different name, and the
    // refusal is about the directory.
    run(
        &home,
        &[
            "apply",
            "--dry-run",
            "--yes",
            "--from",
            &src.display().to_string(),
            "--config",
            &real.join("cfgd.yaml").display().to_string(),
        ],
    )
    .code(1)
    .stderr(predicate::str::contains(
        "Refusing to write into the default config directory",
    ));

    assert!(!real.join(".git").exists(), "nothing was cloned over it");
}

#[test]
fn the_refusal_is_a_classified_error_with_its_two_ways_forward_as_commands() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    source_repo(&src);
    let home = tmp.path().join("home");
    let dest = default_config_dir(&home);
    std::fs::write(dest.join("cfgd.yaml"), "apiVersion: cfgd.io/v1alpha1\n").unwrap();

    // Human mode: the remediation is a `$` block, not prose inside the subject.
    let human = run(
        &home,
        &[
            "--color",
            "never",
            "init",
            "--from",
            &src.display().to_string(),
        ],
    )
    .code(1)
    .get_output()
    .stderr
    .clone();
    let human = String::from_utf8(human).unwrap();
    assert!(
        human.contains("$ cfgd init <dir> --from <source>")
            && human.contains("$ cfgd apply --from <source> --config <dir>/cfgd.yaml"),
        "both ways forward render as commands, got:\n{human}"
    );
    assert!(
        !human.contains("Name a destination (`cfgd init"),
        "the remediation is no longer inside the error subject:\n{human}"
    );

    // `-o json`: a user error, not an unclassified internal one.
    let structured = run(
        &home,
        &["-o", "json", "init", "--from", &src.display().to_string()],
    )
    .code(1)
    .get_output()
    .stdout
    .clone();
    let payload: serde_json::Value =
        serde_json::from_slice(&structured).expect("one error object on stdout");
    assert_eq!(payload["error"], "config_dir_occupied", "got: {payload}");
    assert_eq!(
        payload["finding"], "it already holds a cfgd.yaml",
        "got: {payload}"
    );
}
