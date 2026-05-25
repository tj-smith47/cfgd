//! Snapshot tests for `cfgd module upgrade`.
//!
//! Cases:
//!   - `module_upgrade/no_change.txt` / `.json` — locked ref already
//!     matches HEAD; the command short-circuits with the "already at this
//!     version" Doc carrying `noChange: true`.
//!   - `module_upgrade/cancelled.txt` — new ref differs; the user declines
//!     the prompt; the command emits the "Cancelled" Doc and exits.
//!   - `module_upgrade/happy.txt` / `.json` — new ref differs; user accepts;
//!     lockfile rewritten with the new commit + integrity.
//!
//! No `bridge.txt`: `cmd_module_upgrade` is buffered-only. Outbound git
//! fetches go through a null printer (see `null_lib_printer()` in
//! registry.rs) so the library's progress noise is suppressed; the
//! user-facing surface is the final emit. The bridge invariant does not
//! apply when there is no streaming surface.

mod common;

use std::path::Path;

use cfgd::cli::module;
use cfgd_core::output::{Printer, PromptAnswer};
use cfgd_core::test_helpers::test_printer;
use serial_test::serial;

use common::{cli_for, make_bare_module_repo};

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for inner in chars.by_ref() {
                if inner == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn assert_snapshot(base: &Path, name: &str, actual: &str) {
    let path = base.join(name);
    if std::env::var("INSTA_UPDATE").as_deref() == Ok("always") || !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap();
    pretty_assertions::assert_eq!(actual, expected, "snapshot mismatch: {name}");
}

fn mask_commit_sha(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if i + 40 <= chars.len()
            && chars[i..i + 40]
                .iter()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            && (i == 0 || !chars[i - 1].is_ascii_alphanumeric())
            && (i + 40 == chars.len() || !chars[i + 40].is_ascii_alphanumeric())
        {
            out.push_str("<COMMIT_SHA>");
            i += 40;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn mask_integrity(s: &str) -> String {
    // sha256-<64 base64-or-hex> integrity strings vary every run.
    let mut out = String::new();
    let mut rest = s;
    while let Some(idx) = rest.find("sha256-") {
        out.push_str(&rest[..idx]);
        out.push_str("sha256-<INTEGRITY>");
        let after = &rest[idx + "sha256-".len()..];
        // Drop following base64/hex chars until non-base64.
        let end = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '='))
            .unwrap_or(after.len());
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

fn upgrade_test_setup() -> (tempfile::TempDir, tempfile::TempDir) {
    let config_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(config_dir.path().join("profiles")).unwrap();
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
    (config_dir, state_dir)
}

/// Add a second tag to an existing bare module repo. Returns the second
/// tag string. The new tag points at a fresh commit with a tweaked
/// description, so the upgrade flow sees a real change.
fn push_second_tag(
    tmp_root: &Path,
    module_name: &str,
    bare: &Path,
    new_tag: &str,
) -> std::path::PathBuf {
    let src = tmp_root.join(format!("{module_name}-src2"));
    let bare_url = format!("file://{}", bare.display());
    let repo = git2::Repository::clone(&bare_url, &src).expect("clone bare");
    let yaml = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: {module_name}\n  description: upgraded test mod\nspec: {{}}\n"
    );
    std::fs::write(src.join("module.yaml"), yaml).unwrap();

    let mut index = repo.index().unwrap();
    index.add_path(Path::new("module.yaml")).unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let parent = repo.head().unwrap().peel_to_commit().unwrap();
    let sig = git2::Signature::now("t", "t@example.com").unwrap();
    let commit_id = repo
        .commit(Some("HEAD"), &sig, &sig, "upgrade", &tree, &[&parent])
        .unwrap();
    drop(tree);
    let commit_obj = repo.find_commit(commit_id).unwrap();
    repo.tag(new_tag, commit_obj.as_object(), &sig, "next", false)
        .unwrap();

    let branch = repo
        .head()
        .unwrap()
        .shorthand()
        .unwrap_or("master")
        .to_string();
    let mut remote = repo.find_remote("origin").unwrap();
    remote
        .push(
            &[
                &format!("refs/heads/{branch}:refs/heads/{branch}"),
                &format!("refs/tags/{new_tag}:refs/tags/{new_tag}"),
            ],
            None,
        )
        .unwrap();
    src
}

#[test]
#[serial]
fn module_upgrade_no_change_human_json() {
    // Lock the module at v1.0.0, then call upgrade with the same ref —
    // remote HEAD is the same commit, so the "already at this version"
    // short-circuit fires.
    let (config_dir, _state_dir) = upgrade_test_setup();
    let _home = cfgd_core::with_test_home_guard(config_dir.path());
    let _env = cfgd_core::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");

    let bare_root = tempfile::tempdir().unwrap();
    let bare = make_bare_module_repo(bare_root.path(), "upmod", "v1.0.0");
    let url = format!("file://{}@v1.0.0", bare.display());

    let cli = cli_for(config_dir.path(), config_dir.path());
    let v2a = test_printer();
    // Pre-load the lockfile via add_remote.
    module::cmd_module_add_remote(&cli, &v2a, &url, None, true, true).unwrap();

    let (printer, cap) = Printer::for_test_doc();
    module::cmd_module_upgrade(&cli, &printer, "upmod", Some("v1.0.0"), true, true).unwrap();
    drop(printer);

    let mut stripped =
        strip_ansi(&cap.human()).replace(&config_dir.path().display().to_string(), "<CONFIG_DIR>");
    stripped = mask_commit_sha(&stripped);
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "module_upgrade/no_change.txt",
        &stripped,
    );

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["name"], "upmod");
    assert_eq!(json["noChange"], true);
    assert!(json["commit"].as_str().is_some());
}

#[test]
#[serial]
fn module_upgrade_cancelled_human() {
    // Set up an older locked ref + a newer tag on the bare repo so the
    // diff path runs. Prompt declined → Cancelled Doc.
    let (config_dir, _state_dir) = upgrade_test_setup();
    let _home = cfgd_core::with_test_home_guard(config_dir.path());
    let _env = cfgd_core::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");

    let bare_root = tempfile::tempdir().unwrap();
    let bare = make_bare_module_repo(bare_root.path(), "upmod", "v1.0.0");
    let url = format!("file://{}@v1.0.0", bare.display());

    let cli = cli_for(config_dir.path(), config_dir.path());
    let v2a = test_printer();
    module::cmd_module_add_remote(&cli, &v2a, &url, None, true, true).unwrap();

    // Push a second tag (v1.1.0) onto the bare repo with a fresh commit.
    push_second_tag(bare_root.path(), "upmod", &bare, "v1.1.0");

    let (printer, cap) =
        Printer::for_test_doc_with_prompt_responses(vec![PromptAnswer::Confirm(false)]);
    module::cmd_module_upgrade(&cli, &printer, "upmod", Some("v1.1.0"), false, true).unwrap();
    drop(printer);

    let mut stripped =
        strip_ansi(&cap.human()).replace(&config_dir.path().display().to_string(), "<CONFIG_DIR>");
    stripped = mask_commit_sha(&stripped);
    stripped = mask_integrity(&stripped);
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "module_upgrade/cancelled.txt",
        &stripped,
    );

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["name"], "upmod");
    assert_eq!(json["cancelled"], true);
}

#[test]
#[serial]
fn module_upgrade_happy_human_json() {
    let (config_dir, _state_dir) = upgrade_test_setup();
    let _home = cfgd_core::with_test_home_guard(config_dir.path());
    let _env = cfgd_core::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");

    let bare_root = tempfile::tempdir().unwrap();
    let bare = make_bare_module_repo(bare_root.path(), "upmod", "v1.0.0");
    let url = format!("file://{}@v1.0.0", bare.display());

    let cli = cli_for(config_dir.path(), config_dir.path());
    let v2a = test_printer();
    module::cmd_module_add_remote(&cli, &v2a, &url, None, true, true).unwrap();

    push_second_tag(bare_root.path(), "upmod", &bare, "v1.1.0");

    let (printer, cap) = Printer::for_test_doc();
    module::cmd_module_upgrade(&cli, &printer, "upmod", Some("v1.1.0"), true, true).unwrap();
    drop(printer);

    let mut stripped =
        strip_ansi(&cap.human()).replace(&config_dir.path().display().to_string(), "<CONFIG_DIR>");
    stripped = mask_commit_sha(&stripped);
    stripped = mask_integrity(&stripped);
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "module_upgrade/happy.txt",
        &stripped,
    );

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["name"], "upmod");
    assert_eq!(json["pinnedRef"], "v1.1.0");
    assert!(json["newCommit"].as_str().is_some());
    assert!(json["oldCommit"].as_str().is_some());

    // The JSON payload includes commit hashes that vary every run. Lock the
    // structural-stable fields only by writing a normalized projection.
    let normalized = serde_json::json!({
        "name": json["name"],
        "pinnedRef": json["pinnedRef"],
        "integrity": "<INTEGRITY>",
        "oldCommit": "<OLD_COMMIT>",
        "newCommit": "<NEW_COMMIT>",
    });
    let path = Path::new(SNAPSHOT_ROOT).join("module_upgrade/happy.json");
    let actual = serde_json::to_string_pretty(&normalized).unwrap();
    if std::env::var("INSTA_UPDATE").as_deref() == Ok("always") || !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &actual).unwrap();
    } else {
        let expected = std::fs::read_to_string(&path).unwrap();
        pretty_assertions::assert_eq!(
            actual,
            expected,
            "snapshot mismatch: module_upgrade/happy.json"
        );
    }
}
