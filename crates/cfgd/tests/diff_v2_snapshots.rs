//! Snapshot tests for `cfgd diff`.
//!
//! Real `cmd_diff` capture against tempdir profiles. The file-diff body
//! (subheaders + unified diff lines) still renders through the v1 `Printer`
//! via `CfgdFileManager::diff` until F4b migrates the files lib; v2 snapshots
//! only lock the section headers + outcome statuses + buffered summary, not
//! the diff body. Test profiles use an empty `spec.system` map so each
//! system configurator short-circuits on `merged.system.get(key) == None` —
//! the System section emits only "No system drift" regardless of host. The
//! `normalize` helper handles tempdir path substitution only.
//!
//! Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test diff_v2_snapshots

mod common;

use std::path::{Path, PathBuf};

use cfgd::cli::diff::{build_diff_doc, cmd_diff};
use cfgd::cli::output_types::{DiffOutput, DiffSummary, PackageDrift, SystemDriftOutput};
use cfgd_core::output::{Doc, Printer, Role};
use pretty_assertions::assert_eq;

use common::cli_for;

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

/// Build a profile where the target file already exists with content matching
/// the source — `fm.diff` returns `has_diffs=false`, files section emits "No
/// file drift". Combined with an empty `packages` and empty `system` map this
/// is the clean state.
fn no_drift_setup() -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
    let config_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();

    let files_dir = config_dir.path().join("files");
    std::fs::create_dir_all(&files_dir).unwrap();
    std::fs::write(files_dir.join("hello.txt"), "hello world").unwrap();

    let out_dir = config_dir.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();
    let target = out_dir.join("hello.txt");
    std::fs::write(&target, "hello world").unwrap();

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

/// Profile where the target does NOT exist on disk; `fm.diff` reports drift.
/// Same shape as `common::tiny_profile_setup`, inlined here because we add a
/// custom-manager profile entry in other fixtures and want consistency.
fn file_drift_setup() -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
    let config_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();

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

/// Profile referencing a custom package manager whose `is_installed` script
/// always returns `1` (nothing installed). The profile lists a package under
/// that manager, so `plan_packages` returns an `Install` action.
fn package_drift_setup() -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
    let config_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();

    let files_dir = config_dir.path().join("files");
    std::fs::create_dir_all(&files_dir).unwrap();
    std::fs::write(files_dir.join("hello.txt"), "hello world").unwrap();
    let out_dir = config_dir.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();
    let target = out_dir.join("hello.txt");
    std::fs::write(&target, "hello world").unwrap();

    let profile = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: tiny\nspec:\n  inherits: []\n  modules: []\n  files:\n    managed:\n      - source: files/hello.txt\n        target: {}\n        strategy: Copy\n  packages:\n    custom:\n      - name: drift-mgr\n        check: \"true\"\n        listInstalled: \"true\"\n        install: \"true\"\n        uninstall: \"true\"\n        packages:\n          - drifted-pkg\n",
        target.display()
    );
    let profiles_dir = config_dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(profiles_dir.join("tiny.yaml"), &profile).unwrap();

    let config = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: tiny\n";
    std::fs::write(config_dir.path().join("cfgd.yaml"), config).unwrap();

    (config_dir, state_dir, target)
}

/// Profile referencing a module with files (target missing → file drift) and
/// no packages. Drives the `--module` branch in `cmd_diff`.
fn module_only_setup() -> (tempfile::TempDir, tempfile::TempDir) {
    let config_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();

    let module_dir = config_dir.path().join("modules").join("diff-mod");
    let module_files = module_dir.join("files");
    std::fs::create_dir_all(&module_files).unwrap();
    std::fs::write(module_files.join("conf"), "module content\n").unwrap();

    let module_target = config_dir.path().join("mod-out").join("conf");
    let module_yaml = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: diff-mod\nspec:\n  packages: []\n  files:\n    - source: conf\n      target: {}\n",
        module_target.display()
    );
    std::fs::write(module_dir.join("module.yaml"), module_yaml).unwrap();

    let profile = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: tiny\nspec:\n  inherits: []\n  modules: []\n";
    let profiles_dir = config_dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(profiles_dir.join("tiny.yaml"), profile).unwrap();

    let config = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: tiny\n";
    std::fs::write(config_dir.path().join("cfgd.yaml"), config).unwrap();

    (config_dir, state_dir)
}

/// Real `cmd_diff` against a clean profile — files match, no packages, no
/// system spec. Locks the "No file drift" / "No package drift" / "No system
/// drift" + "No drift detected" summary line.
#[test]
fn diff_no_drift_human() {
    let (config_dir, state_dir, target) = no_drift_setup();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (v2_printer, cap) = Printer::for_test_doc();

    cmd_diff(&cli, &v2_printer, None, false).unwrap();
    drop(v2_printer);

    let normalized = normalize(&cap.human(), config_dir.path(), &[(&target, "<TARGET>")]);
    let stripped = strip_ansi(&normalized);
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "diff/no_drift.txt", &stripped);
}

/// JSON payload roundtrip — DiffOutput shape via build_diff_doc + cap.json().
#[test]
fn diff_no_drift_json() {
    let output = DiffOutput {
        packages: Vec::new(),
        system: Vec::new(),
        summary: DiffSummary {
            has_file_drift: false,
            has_pkg_drift: false,
            has_system_drift: false,
        },
    };
    let (v2_printer, cap) = Printer::for_test_doc();
    v2_printer.emit(build_diff_doc(&output));
    drop(v2_printer);

    let expected = serde_json::to_value(&output).unwrap();
    let actual = cap.json().expect("diff doc carries a payload");
    assert_eq!(
        actual, expected,
        "emit -o json must match serde_json::to_value(DiffOutput)"
    );
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "diff/no_drift.json");
}

/// Profile target doesn't exist on disk — `fm.diff` reports drift. Snapshot
/// locks the "Files" section header + summary outcome, not the diff body
/// (which renders through v1 printer; F4b migrates).
#[test]
fn diff_file_drift_human() {
    let (config_dir, state_dir, target) = file_drift_setup();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (v2_printer, cap) = Printer::for_test_doc();

    cmd_diff(&cli, &v2_printer, None, false).unwrap();
    drop(v2_printer);

    let normalized = normalize(&cap.human(), config_dir.path(), &[(&target, "<TARGET>")]);
    let stripped = strip_ansi(&normalized);
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "diff/file_drift.txt", &stripped);
}

/// Profile declares a custom package manager whose `is_installed` script
/// always returns nothing — `plan_packages` emits an `Install` action; the
/// snapshot locks the "Packages: drift-mgr: missing — ..." status line.
#[test]
fn diff_package_drift_human() {
    let (config_dir, state_dir, target) = package_drift_setup();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (v2_printer, cap) = Printer::for_test_doc();

    cmd_diff(&cli, &v2_printer, None, false).unwrap();
    drop(v2_printer);

    let normalized = normalize(&cap.human(), config_dir.path(), &[(&target, "<TARGET>")]);
    let stripped = strip_ansi(&normalized);
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "diff/package_drift.txt",
        &stripped,
    );
}

/// Synthetic system-drift payload — exercises the `SystemDriftOutput` rendering
/// path in `build_diff_doc`. Real system drift requires platform-privileged
/// state mutation (sysctl, launchd, etc.) that is intractable from an
/// integration test, so we anchor the buffered summary's role + payload shape
/// via the pure Doc constructor. The streaming-side rendering of system
/// drift status lines is exercised by the `print_package_drift_v2`-shaped
/// in-module tests (drift.rs) and the F0 renderer bucket-g anchors.
#[test]
fn diff_system_drift_human() {
    let output = DiffOutput {
        packages: Vec::new(),
        system: vec![SystemDriftOutput {
            key: "sysctl.kernel.somaxconn".to_string(),
            expected: "4096".to_string(),
            actual: "128".to_string(),
        }],
        summary: DiffSummary {
            has_file_drift: false,
            has_pkg_drift: false,
            has_system_drift: true,
        },
    };
    let (v2_printer, cap) = Printer::for_test_doc();
    v2_printer.emit(build_diff_doc(&output));
    drop(v2_printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "diff/system_drift.txt", &stripped);
}

/// `--module <name>` branch of `cmd_diff` — drives the parallel Files +
/// Packages sections via resolved-module data and skips the system section.
#[test]
fn diff_module_only_human() {
    let (config_dir, state_dir) = module_only_setup();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (v2_printer, cap) = Printer::for_test_doc();

    cmd_diff(&cli, &v2_printer, Some("diff-mod"), false).unwrap();
    drop(v2_printer);

    let normalized = normalize(&cap.human(), config_dir.path(), &[]);
    let stripped = strip_ansi(&normalized);
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "diff/module_only.txt", &stripped);
}

/// Bridge invariant: streaming sections drop, then buffered Doc emits — the
/// combined human surface contains exactly one blank line at the transition.
/// Uses the `package_drift_setup` shape so both sides have human content.
#[test]
fn diff_bridge_one_blank_line() {
    let output = DiffOutput {
        packages: vec![PackageDrift {
            manager: "drift-mgr".to_string(),
            shape: "missing".to_string(),
            packages: vec!["pkg-a".to_string()],
            bootstrap_method: None,
        }],
        system: Vec::new(),
        summary: DiffSummary {
            has_file_drift: false,
            has_pkg_drift: true,
            has_system_drift: false,
        },
    };
    let (v2_printer, cap) = Printer::for_test_doc();

    v2_printer.heading("Diff");
    {
        let pkg_sec = v2_printer.section("Packages");
        pkg_sec
            .status(Role::Warn, "drift-mgr: missing")
            .detail("pkg-a");
    }

    let doc = Doc::new()
        .status(Role::Warn, "Drift detected")
        .with_data(&output);
    v2_printer.emit(doc);
    drop(v2_printer);

    let captured = strip_ansi(&cap.human());
    assert!(
        captured.contains("\n\n"),
        "bridge missing blank line:\n{captured}"
    );
    assert!(
        !captured.contains("\n\n\n"),
        "bridge has duplicate blank line:\n{captured}"
    );

    assert_snapshot(Path::new(SNAPSHOT_ROOT), "diff/bridge.txt", &captured);
}

// ─────────────────────────────────────────────────────
// snapshot helpers
// ─────────────────────────────────────────────────────

/// Replace tempdir-rooted paths so goldens are host-stable.
fn normalize(raw: &str, config_dir: &Path, extra_paths: &[(&Path, &str)]) -> String {
    let mut out = raw.to_string();
    let cfg_file = config_dir.join("cfgd.yaml");
    out = out.replace(
        &cfg_file.to_string_lossy().to_string(),
        "<CONFIG_DIR>/cfgd.yaml",
    );
    for (path, placeholder) in extra_paths {
        out = out.replace(&path.to_string_lossy().to_string(), placeholder);
    }
    out = out.replace(&config_dir.to_string_lossy().to_string(), "<CONFIG_DIR>");
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
    pretty_assertions::assert_eq!(actual, &expected, "snapshot mismatch: {name}");
}

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
