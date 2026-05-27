//! Snapshot tests for `cfgd module registry {add,remove,rename,list}`,
//! `cfgd module add <url|registry/mod>`, `cfgd module upgrade`, and
//! `cfgd module search`.
//!
//! Cases (registry CRUD — pure buffered):
//!   - `module_registry_add/happy.{txt,json}` — registry written to cfgd.yaml.
//!   - `module_registry_add/already_configured.txt` — idempotent re-add.
//!   - `module_registry_remove/happy.{txt,json}` — registry removed.
//!   - `module_registry_remove/not_found.txt` — removing missing registry.
//!   - `module_registry_rename/happy.{txt,json}` — registry renamed.
//!   - `module_registry_list/empty.{txt,json}` — no registries.
//!   - `module_registry_list/with_entries.{txt,json}` — multiple registries.
//!
//! Cases (streaming-bearing — bridge.txt one-blank-line invariant):
//!   - `module_add/bridge.txt` — `cmd_module_add_remote` against a bare repo
//!     emits the clone spinner + buffered summary with exactly one blank line
//!     between them.
//!   - `module_add_from_registry/bridge.txt` — `cmd_module_add_from_registry`
//!     resolves via registry, drives `cmd_module_add_remote`.
//!
//! Cases (search):
//!   - `module_search/happy.{txt,json}` — local registry fixture, one match.
//!   - `module_search/no_registries.{txt,json}` — config exists but registries empty.
//!
//! Goldens live under `tests/output_snapshots/`. Regenerate via:
//!     INSTA_UPDATE=always cargo test -p cfgd --test module_registry_snapshots

mod common;

use std::path::Path;

use cfgd::cli::module;
use cfgd_core::output::{Printer, PromptAnswer};
use cfgd_core::test_helpers::assert_snapshot_golden as assert_snapshot;
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

fn registry_test_setup() -> (tempfile::TempDir, tempfile::TempDir) {
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

fn normalize(raw: &str, config_dir: &Path) -> String {
    cfgd_core::normalize_for_snapshot(raw, &[(config_dir, "<CONFIG_DIR>")])
}

/// Replace any 40-char lowercase-hex run (a git commit SHA) with the literal
/// placeholder `<COMMIT_SHA>` so snapshots are stable across runs. Walks
/// chars (not bytes) so multi-byte UTF-8 glyphs survive intact.
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

// ─── registry add ───────────────────────────────────────────────────

#[test]
fn module_registry_add_happy_human() {
    let (config_dir, _state_dir) = registry_test_setup();
    let cli = cli_for(config_dir.path(), config_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    module::cmd_module_registry_add(
        &cli,
        &printer,
        "https://github.com/team/modules.git",
        Some("team"),
    )
    .unwrap();
    drop(printer);

    let stripped = normalize(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "module_registry_add/happy.txt",
        &stripped,
    );
}

#[test]
fn module_registry_add_happy_json() {
    let (config_dir, _state_dir) = registry_test_setup();
    let cli = cli_for(config_dir.path(), config_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    module::cmd_module_registry_add(
        &cli,
        &printer,
        "https://github.com/team/modules.git",
        Some("team"),
    )
    .unwrap();
    drop(printer);

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["name"], "team");
    assert_eq!(json["alreadyConfigured"], false);
}

#[test]
fn module_registry_add_already_configured_human() {
    let (config_dir, _state_dir) = registry_test_setup();
    let cli = cli_for(config_dir.path(), config_dir.path());

    // First add
    let v2a = test_printer();
    module::cmd_module_registry_add(
        &cli,
        &v2a,
        "https://github.com/team/modules.git",
        Some("team"),
    )
    .unwrap();

    // Second add hits the already-configured Doc.
    let (printer, cap) = Printer::for_test_doc();
    module::cmd_module_registry_add(
        &cli,
        &printer,
        "https://github.com/team/different.git",
        Some("team"),
    )
    .unwrap();
    drop(printer);

    let stripped = normalize(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "module_registry_add/already_configured.txt",
        &stripped,
    );
}

// ─── registry remove ────────────────────────────────────────────────

#[test]
fn module_registry_remove_happy_human() {
    let (config_dir, _state_dir) = registry_test_setup();
    let cli = cli_for(config_dir.path(), config_dir.path());
    let v2a = test_printer();
    module::cmd_module_registry_add(&cli, &v2a, "https://example.com/reg.git", Some("team"))
        .unwrap();

    let (printer, cap) = Printer::for_test_doc();
    module::cmd_module_registry_remove(&cli, &printer, "team").unwrap();
    drop(printer);

    let stripped = normalize(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "module_registry_remove/happy.txt",
        &stripped,
    );

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["name"], "team");
    assert_eq!(json["outcome"], "removed");
}

#[test]
fn module_registry_remove_not_found_human() {
    let (config_dir, _state_dir) = registry_test_setup();
    let cli = cli_for(config_dir.path(), config_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    module::cmd_module_registry_remove(&cli, &printer, "ghost").unwrap();
    drop(printer);

    let stripped = normalize(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "module_registry_remove/not_found.txt",
        &stripped,
    );
}

// ─── registry rename ────────────────────────────────────────────────

#[test]
fn module_registry_rename_happy_human() {
    let (config_dir, _state_dir) = registry_test_setup();
    let cli = cli_for(config_dir.path(), config_dir.path());
    let v2a = test_printer();
    module::cmd_module_registry_add(&cli, &v2a, "https://example.com/r.git", Some("old")).unwrap();

    let (printer, cap) = Printer::for_test_doc();
    module::cmd_module_registry_rename(&cli, &printer, "old", "fresh").unwrap();
    drop(printer);

    let stripped = normalize(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "module_registry_rename/happy.txt",
        &stripped,
    );

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["from"], "old");
    assert_eq!(json["to"], "fresh");
    cap.assert_json_snapshot_in(
        Path::new(SNAPSHOT_ROOT),
        "module_registry_rename/happy.json",
    );
}

// ─── registry list ──────────────────────────────────────────────────

#[test]
fn module_registry_list_empty_human() {
    let (config_dir, _state_dir) = registry_test_setup();
    let cli = cli_for(config_dir.path(), config_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    module::cmd_module_registry_list(&cli, &printer).unwrap();
    drop(printer);

    let stripped = normalize(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "module_registry_list/empty.txt",
        &stripped,
    );

    let json = cap.json().expect("doc captured json");
    assert!(json.is_array());
    assert_eq!(json.as_array().unwrap().len(), 0);
}

#[test]
fn module_registry_list_with_entries_human() {
    let (config_dir, _state_dir) = registry_test_setup();
    let cli = cli_for(config_dir.path(), config_dir.path());
    let v2a = test_printer();
    module::cmd_module_registry_add(&cli, &v2a, "https://example.com/a.git", Some("alpha"))
        .unwrap();
    module::cmd_module_registry_add(&cli, &v2a, "https://example.com/b.git", Some("beta")).unwrap();

    let (printer, cap) = Printer::for_test_doc();
    module::cmd_module_registry_list(&cli, &printer).unwrap();
    drop(printer);

    let stripped = normalize(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "module_registry_list/with_entries.txt",
        &stripped,
    );

    let json = cap.json().expect("doc captured json");
    assert!(json.is_array());
    assert_eq!(json.as_array().unwrap().len(), 2);
}

// ─── module add (streaming) — bridge.txt ────────────────────────────

#[test]
#[serial]
fn module_add_bridge_one_blank_line() {
    // Bridge invariant: the streaming clone surface (spinner) and
    // the buffered summary (lock summary Doc) must be separated by
    // exactly one blank line in the human render — no more, no fewer.
    let (config_dir, _state_dir) = registry_test_setup();
    let _home = cfgd_core::with_test_home_guard(config_dir.path());
    let _env = cfgd_core::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");

    let bare_root = tempfile::tempdir().unwrap();
    let bare = make_bare_module_repo(bare_root.path(), "bridgemod", "v1.0.0");
    let url = format!("{}@v1.0.0", cfgd_core::to_file_url(&bare));

    let cli = cli_for(config_dir.path(), config_dir.path());
    let (printer, cap) =
        Printer::for_test_doc_with_prompt_responses(vec![PromptAnswer::Confirm(true)]);
    module::cmd_module_add_remote(&cli, &printer, &url, None, true, true).unwrap();
    drop(printer);

    let combined = cap.human();
    assert!(
        combined.contains("\n\n"),
        "bridge missing blank line: {combined}"
    );
    assert!(
        !combined.contains("\n\n\n"),
        "bridge has duplicate blank line: {combined}"
    );

    let stripped = cfgd_core::normalize_for_snapshot(
        &strip_ansi(&combined),
        &[
            (&bare, "<BARE>"),
            (bare_root.path(), "<BARE_ROOT>"),
            (config_dir.path(), "<CONFIG_DIR>"),
        ],
    );
    // The `to_file_url` helper emits `file:///<absolute-posix-path>` on every
    // OS — three slashes always — so on Linux the unix bare path `/tmp/...`
    // collapses cleanly to `file://<BARE>` once the leading `/` of the path
    // is consumed by the substitution, while on Windows the bare path
    // `C:/Users/...` has no leading `/`, leaving the third slash from the
    // URL prefix in place (`file:///<BARE>`). Fold the Windows form to the
    // unix shape so a single golden survives both platforms.
    let stripped = stripped.replace("file:///<BARE>", "file://<BARE>");
    let stripped = mask_commit_sha(&stripped);
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "module_add/bridge.txt", &stripped);
}

#[test]
#[serial]
fn module_add_happy_json() {
    let (config_dir, _state_dir) = registry_test_setup();
    let _home = cfgd_core::with_test_home_guard(config_dir.path());
    let _env = cfgd_core::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");

    let bare_root = tempfile::tempdir().unwrap();
    let bare = make_bare_module_repo(bare_root.path(), "jsonmod", "v1.0.0");
    let url = format!("{}@v1.0.0", cfgd_core::to_file_url(&bare));

    let cli = cli_for(config_dir.path(), config_dir.path());
    let (printer, cap) =
        Printer::for_test_doc_with_prompt_responses(vec![PromptAnswer::Confirm(true)]);
    module::cmd_module_add_remote(&cli, &printer, &url, None, true, true).unwrap();
    drop(printer);

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["name"], "jsonmod");
    assert_eq!(json["pinnedRef"], "v1.0.0");
    assert!(json["commit"].as_str().is_some());
    assert!(json["integrity"].as_str().is_some());
}

// ─── module add_from_registry (streaming) — bridge.txt ──────────────

#[test]
#[serial]
fn module_add_from_registry_bridge_one_blank_line() {
    // The registry resolver assembles a synthetic git URL and delegates
    // to cmd_module_add_remote, so we get the same streaming clone +
    // buffered summary bridge from a one-step-removed entry point.
    let (config_dir, _state_dir) = registry_test_setup();
    let _home = cfgd_core::with_test_home_guard(config_dir.path());
    let _env = cfgd_core::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");

    let src_root = tempfile::tempdir().unwrap();
    let src = init_registry_source_for_test(src_root.path(), "alpha", "1.0.0", "Alpha module");
    let reg_url = cfgd_core::to_file_url(&src);

    // Wire the registry into cfgd.yaml.
    let yaml = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n  modules:\n    registries:\n      - name: myreg\n        url: {reg_url}\n"
    );
    std::fs::write(config_dir.path().join("cfgd.yaml"), yaml).unwrap();

    let cli = cli_for(config_dir.path(), config_dir.path());
    let (printer, cap) =
        Printer::for_test_doc_with_prompt_responses(vec![PromptAnswer::Confirm(true)]);
    module::cmd_module_add_from_registry(&cli, &printer, "myreg/alpha@v1.0.0", true, true).unwrap();
    drop(printer);

    let combined = cap.human();
    assert!(
        combined.contains("\n\n"),
        "bridge missing blank line: {combined}"
    );
    assert!(
        !combined.contains("\n\n\n"),
        "bridge has duplicate blank line: {combined}"
    );

    let stripped = cfgd_core::normalize_for_snapshot(
        &strip_ansi(&combined),
        &[
            (&src, "<REG_SRC>"),
            (src_root.path(), "<REG_ROOT>"),
            (config_dir.path(), "<CONFIG_DIR>"),
        ],
    );
    // `to_file_url` emits `file:///<absolute-posix-path>` on every OS; on
    // Windows the bare path lacks a leading `/`, leaving the URL prefix's
    // third slash visible. Fold that to the unix shape (see the matching
    // comment in `module_add_bridge_one_blank_line`).
    let stripped = stripped.replace("file:///<REG_SRC>", "file://<REG_SRC>");
    let stripped = mask_commit_sha(&stripped);
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "module_add_from_registry/bridge.txt",
        &stripped,
    );
}

// ─── search ─────────────────────────────────────────────────────────

#[test]
fn module_search_no_registries_human() {
    let (config_dir, _state_dir) = registry_test_setup();
    let cli = cli_for(config_dir.path(), config_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    module::cmd_module_search(&cli, &printer, "anything").unwrap();
    drop(printer);

    let stripped = normalize(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "module_search/no_registries.txt",
        &stripped,
    );

    let json = cap.json().expect("doc captured json");
    assert!(json.is_array());
    assert_eq!(json.as_array().unwrap().len(), 0);
}

#[test]
#[serial]
fn module_search_happy_json() {
    let (config_dir, _state_dir) = registry_test_setup();
    let _home = cfgd_core::with_test_home_guard(config_dir.path());
    let _env = cfgd_core::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");

    let src_root = tempfile::tempdir().unwrap();
    let src = init_registry_source_for_test(src_root.path(), "alpha", "1.0.0", "Alpha module");
    let reg_url = cfgd_core::to_file_url(&src);

    let yaml = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n  modules:\n    registries:\n      - name: myreg\n        url: {reg_url}\n"
    );
    std::fs::write(config_dir.path().join("cfgd.yaml"), yaml).unwrap();

    let cli = cli_for(config_dir.path(), config_dir.path());
    let (printer, cap) = Printer::for_test_doc();
    module::cmd_module_search(&cli, &printer, "alpha").unwrap();
    drop(printer);

    let json = cap.json().expect("doc captured json");
    let arr = json.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], "myreg/alpha");
    assert_eq!(arr[0]["registry"], "myreg");
}

// Stand up a non-bare registry source repo with `modules/<mod>/module.yaml`
// committed and tagged as `<mod>/v<version>`. Mirrors the helper in the
// in-crate `cmd_module_add_from_registry_local` tests; reproduced here so
// snapshot tests don't pull in private test internals.
fn init_registry_source_for_test(
    tmp_root: &Path,
    module_name: &str,
    version: &str,
    description: &str,
) -> std::path::PathBuf {
    let src = tmp_root.join("registry-src");
    let repo = git2::Repository::init(&src).expect("init registry repo");

    let mod_dir = src.join("modules").join(module_name);
    std::fs::create_dir_all(&mod_dir).unwrap();
    let yaml = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: {module_name}\n  description: {description}\nspec: {{}}\n"
    );
    std::fs::write(mod_dir.join("module.yaml"), yaml).unwrap();

    let mut index = repo.index().unwrap();
    index
        .add_path(&Path::new("modules").join(module_name).join("module.yaml"))
        .unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let sig = git2::Signature::now("t", "t@example.com").unwrap();
    let commit_id = repo
        .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
        .unwrap();
    drop(tree);
    let commit_obj = repo.find_commit(commit_id).unwrap();
    let tag = format!("{module_name}/v{version}");
    repo.tag(&tag, commit_obj.as_object(), &sig, "release", false)
        .unwrap();
    src
}
