//! Snapshot tests for `cfgd module search`.
//!
//! Cases:
//!   - `module_search/bridge.txt` — per-source spinner streaming surface
//!     and the buffered Doc summary are separated by exactly one blank
//!     line (one-blank-line bridge invariant).
//!   - `module_search/happy.txt` — query matches a registry fixture; one
//!     result row in the buffered table.
//!
//! `module_search/no_registries.txt` + the `happy_json` JSON-shape check
//! live alongside the other registry snapshot tests in
//! `module_registry_v2_snapshots.rs` (historical grouping). The bridge
//! case is split out here because the streaming-bearing surface deserves
//! a standalone bridge anchor.

mod common;

use std::path::Path;

use cfgd::cli::module;
use cfgd_core::output::Printer;
use serial_test::serial;

use common::cli_for;

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

fn search_test_setup() -> (tempfile::TempDir, tempfile::TempDir) {
    let config_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(config_dir.path().join("profiles")).unwrap();
    (config_dir, state_dir)
}

/// Stand up a non-bare registry source repo with `modules/<mod>/module.yaml`
/// committed and tagged. Mirrors the helper in module_registry_v2_snapshots.
fn init_registry_source(
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

fn write_registry_config(config_dir: &Path, reg_url: &str) {
    let yaml = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n  modules:\n    registries:\n      - name: myreg\n        url: {reg_url}\n"
    );
    std::fs::write(config_dir.join("cfgd.yaml"), yaml).unwrap();
    std::fs::write(
        config_dir.join("profiles/default.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
    )
    .unwrap();
}

#[test]
#[serial]
fn module_search_bridge_one_blank_line() {
    // Bridge invariant: the per-registry spinner stream and the
    // buffered table Doc must have exactly one blank line between them.
    let (config_dir, _state_dir) = search_test_setup();
    let _home = cfgd_core::with_test_home_guard(config_dir.path());
    let _env = cfgd_core::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");

    let src_root = tempfile::tempdir().unwrap();
    let src = init_registry_source(src_root.path(), "alpha", "1.0.0", "Alpha module");
    let reg_url = format!("file://{}", src.display());
    write_registry_config(config_dir.path(), &reg_url);

    let cli = cli_for(config_dir.path(), config_dir.path());
    let (printer, cap) = Printer::for_test_doc();
    module::cmd_module_search(&cli, &printer, "alpha").unwrap();
    drop(printer);

    let combined = cap.human();
    assert!(
        combined.contains("\n\n"),
        "bridge missing blank line:\n{combined}"
    );
    assert!(
        !combined.contains("\n\n\n"),
        "bridge has duplicate blank line:\n{combined}"
    );

    let mut stripped = strip_ansi(&combined);
    stripped = stripped.replace(&reg_url, "<REG_URL>");
    stripped = stripped.replace(&src.display().to_string(), "<REG_SRC>");
    stripped = stripped.replace(&src_root.path().display().to_string(), "<REG_ROOT>");
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "module_search/bridge.txt",
        &stripped,
    );
}

#[test]
#[serial]
fn search_happy_human() {
    let (config_dir, _state_dir) = search_test_setup();
    let _home = cfgd_core::with_test_home_guard(config_dir.path());
    let _env = cfgd_core::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");

    let src_root = tempfile::tempdir().unwrap();
    let src = init_registry_source(src_root.path(), "alpha", "1.0.0", "Alpha module");
    let reg_url = format!("file://{}", src.display());
    write_registry_config(config_dir.path(), &reg_url);

    let cli = cli_for(config_dir.path(), config_dir.path());
    let (printer, cap) = Printer::for_test_doc();
    module::cmd_module_search(&cli, &printer, "alpha").unwrap();
    drop(printer);

    // Normalize variable paths so the golden is host-stable.
    let mut stripped = strip_ansi(&cap.human());
    stripped = stripped.replace(&reg_url, "<REG_URL>");
    stripped = stripped.replace(&src.display().to_string(), "<REG_SRC>");
    stripped = stripped.replace(&src_root.path().display().to_string(), "<REG_ROOT>");
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "module_search/happy.txt",
        &stripped,
    );

    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "module_search/happy.json");
}
