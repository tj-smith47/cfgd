//! A config `cfgd init` writes is aligned to the build that wrote it.
//!
//! The load-time migration gate runs before dispatch, when the document
//! `init` is about to write does not exist yet; init runs the same gate once
//! the document is on disk, under the same `--yes` and terminal rules. These
//! run the real binary, because what they claim is what the next command in
//! the same home prints.

use std::path::Path;

use assert_cmd::prelude::*;
use predicates::prelude::*;

mod cfgd_binary;
use cfgd_binary::cfgd_bin;

/// A source config behind this build's schema by exactly `spec.migrationPolicy`.
const BEHIND: &str = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: from-src\nspec:\n  profile: base\n  fileStrategy: Symlink\n";

const PROFILE: &str =
    "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: base\nspec: {}\n";

/// What the gate says of [`BEHIND`] when it only reports.
const WARN_LINE: &str =
    "This config does not declare 1 field this build reads (spec.migrationPolicy)";

/// What the gate says of [`BEHIND`] once it has written the field.
const ALIGNED_LINE: &str = "Added 1 field this build reads to your config (spec.migrationPolicy)";

/// A committed git repository holding [`BEHIND`] and the profile it names.
fn source_repo(dir: &Path) {
    std::fs::create_dir_all(dir.join("profiles")).unwrap();
    std::fs::write(dir.join("cfgd.yaml"), BEHIND).unwrap();
    std::fs::write(dir.join("profiles/base.yaml"), PROFILE).unwrap();
    let repo = git2::Repository::init(dir).unwrap();
    let sig = git2::Signature::now("Test", "test@example.com").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("cfgd.yaml")).unwrap();
    index.add_path(Path::new("profiles/base.yaml")).unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .unwrap();
}

fn stderr_of(assert: &assert_cmd::assert::Assert) -> String {
    String::from_utf8_lossy(&assert.get_output().stderr).into_owned()
}

fn stdout_of(assert: &assert_cmd::assert::Assert) -> String {
    String::from_utf8_lossy(&assert.get_output().stdout).into_owned()
}

#[test]
fn init_with_yes_aligns_the_config_it_cloned_and_the_next_command_says_nothing_about_it() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    source_repo(&src);
    let dest = tmp.path().join("dest");
    let dest_str = dest.display().to_string();

    let init = cfgd_bin()
        .unwrap()
        .args([
            "init",
            &dest_str,
            "--from",
            &src.display().to_string(),
            "--yes",
        ])
        .env("CFGD_ALLOW_LOCAL_SOURCES", "1")
        .assert()
        .success();
    let said = stderr_of(&init);
    assert_eq!(
        said.matches(ALIGNED_LINE).count(),
        1,
        "init says once that it wrote the field: {said}"
    );

    let config = dest.join("cfgd.yaml");
    let written = std::fs::read_to_string(&config).unwrap();
    let doc: serde_yaml::Value = serde_yaml::from_str(&written).unwrap();
    assert_eq!(
        doc["spec"]["migrationPolicy"],
        serde_yaml::Value::String("Prompt".to_string()),
        "the field is on disk: {written}"
    );

    let status = cfgd_bin()
        .unwrap()
        .args(["--config", &config.display().to_string(), "status"])
        .assert();
    let (out, err) = (stdout_of(&status), stderr_of(&status));
    for text in [&out, &err] {
        assert!(
            !text.contains("this build reads"),
            "the next command neither asks nor warns: stdout={out} stderr={err}"
        );
    }
}

#[test]
fn init_without_yes_off_a_terminal_warns_and_leaves_the_cloned_config_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    source_repo(&src);
    let dest = tmp.path().join("dest");

    let init = cfgd_bin()
        .unwrap()
        .args([
            "init",
            &dest.display().to_string(),
            "--from",
            &src.display().to_string(),
        ])
        .env("CFGD_ALLOW_LOCAL_SOURCES", "1")
        .assert()
        .success()
        .stderr(predicate::str::contains(ALIGNED_LINE).not());
    let said = stderr_of(&init);
    assert_eq!(
        said.matches(WARN_LINE).count(),
        1,
        "init reports the field once, and the load-time gate stays out of it: {said}"
    );
    assert_eq!(
        std::fs::read_to_string(dest.join("cfgd.yaml")).unwrap(),
        BEHIND,
        "a report writes nothing"
    );
}

/// Every spelling of yes the verb itself accepts takes the load-time gate's
/// prompt too: the gate reads `--yes` off the same parse the command does.
#[test]
fn every_spelling_of_yes_the_parser_accepts_takes_the_load_time_prompt() {
    let spellings: &[(&[&str], Option<&str>)] = &[
        (&["--yes"], None),
        (&["-qy"], None),
        (&[], Some("1")),
        (&[], Some("true")),
    ];
    for (flags, env) in spellings {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("cfgd.yaml");
        std::fs::write(&config, BEHIND).unwrap();
        std::fs::create_dir_all(tmp.path().join("profiles")).unwrap();
        std::fs::write(tmp.path().join("profiles/base.yaml"), PROFILE).unwrap();

        let mut cmd = cfgd_bin().unwrap();
        cmd.args(["--config", &config.display().to_string()])
            .args(*flags)
            .arg("status");
        if let Some(value) = env {
            cmd.env("CFGD_YES", value);
        }
        cmd.assert().success();

        let doc: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(
            doc["spec"]["migrationPolicy"],
            serde_yaml::Value::String("Prompt".to_string()),
            "flags {flags:?}, CFGD_YES={env:?}: the answer is yes, so the field is written"
        );
    }
}

/// What `config migrate` prints of a document declaring every field.
const DECLARES_EVERY_FIELD: &str = "Config declares every field this build reads";

#[test]
fn a_config_set_that_creates_a_section_leaves_nothing_for_the_next_migrate_to_name() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    source_repo(&src);
    let dest = tmp.path().join("dest");
    cfgd_bin()
        .unwrap()
        .args([
            "init",
            &dest.display().to_string(),
            "--from",
            &src.display().to_string(),
            "--yes",
        ])
        .env("CFGD_ALLOW_LOCAL_SOURCES", "1")
        .assert()
        .success();
    let config = dest.join("cfgd.yaml");
    let config_str = config.display().to_string();
    assert!(
        !std::fs::read_to_string(&config)
            .unwrap()
            .contains("daemon:"),
        "the document starts with no daemon section for the write to create"
    );

    cfgd_bin()
        .unwrap()
        .args([
            "--config",
            &config_str,
            "config",
            "set",
            "daemon.reconcile.autoApply",
            "true",
        ])
        .assert()
        .success();

    let migrate = cfgd_bin()
        .unwrap()
        .args(["--config", &config_str, "config", "migrate"])
        .assert()
        .success();
    let (out, err) = (stdout_of(&migrate), stderr_of(&migrate));
    assert!(
        format!("{out}{err}").contains(DECLARES_EVERY_FIELD),
        "the section the write created declares its siblings: stdout={out} stderr={err}"
    );
}
