//! Snapshot tests for `cfgd init`.
//!
//! Three cases:
//!   - `init/happy.{txt,json}` — fresh scaffold to a new directory, no
//!     `--apply`. Exercises the streaming Phase A (scaffold status lines +
//!     git-init success) and the buffered Phase C "Next steps" Doc, so the
//!     streaming → buffered bridge holds its one-blank-line invariant under
//!     real init data.
//!   - `init/already_initialized.txt` — target dir already has `cfgd.yaml`;
//!     pins the short-circuit status line + the buffered "no-section" Doc
//!     emit path.
//!   - `init/with_apply_streaming_to_buffered.txt` — `--apply --dry-run` on
//!     a scaffolded dir with an empty profile. Exercises the full Phase A →
//!     Phase B → Phase C path, asserting the bridge survives the apply-step
//!     plan table + "Nothing to do" status before the trailing buffered Doc.
//!
//! Goldens live under `tests/output_snapshots/init/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test init_v2_snapshots

use std::path::Path;

use cfgd::cli::init::{InitArgs, cmd_init};
use cfgd_core::output_v2::Printer;

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

#[test]
fn init_happy_human() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("happy-cfg");
    let target_str = target.to_string_lossy().into_owned();
    let args = InitArgs {
        path: Some(&target_str),
        from: None,
        branch: "master",
        name: Some("happy-cfg"),
        apply: false,
        dry_run: false,
        yes: true,
        install_daemon: false,
        theme: None,
        apply_profile: None,
        apply_modules: &[],
    };

    let (old_printer, _sink) = cfgd_core::output::Printer::for_test();
    let (printer, cap) = Printer::for_test_doc();
    cmd_init(&old_printer, &printer, &args).unwrap();
    drop(printer);

    // Replace the tempdir-rooted target with a stable placeholder so the
    // golden survives across hosts (tempfile paths embed `/tmp/.tmpXXXX`).
    let human = cap.human().replace(&target_str, "<TARGET_DIR>");
    let normalized = strip_ansi(&human);
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "init/happy.txt", &normalized);
}

#[test]
fn init_happy_json() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("happy-cfg-json");
    let target_str = target.to_string_lossy().into_owned();
    let args = InitArgs {
        path: Some(&target_str),
        from: None,
        branch: "master",
        name: Some("happy-cfg-json"),
        apply: false,
        dry_run: false,
        yes: true,
        install_daemon: false,
        theme: None,
        apply_profile: None,
        apply_modules: &[],
    };

    let (old_printer, _sink) = cfgd_core::output::Printer::for_test();
    let (printer, cap) = Printer::for_test_doc();
    cmd_init(&old_printer, &printer, &args).unwrap();
    drop(printer);

    let mut json = cap.json().expect("init emits a Doc with payload");
    // Normalize the embedded target_dir so the JSON golden is host-stable.
    if let Some(obj) = json.as_object_mut() {
        obj.insert(
            "target_dir".into(),
            serde_json::Value::String("<TARGET_DIR>".into()),
        );
    }
    let actual = serde_json::to_string_pretty(&json).unwrap();
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "init/happy.json", &actual);
}

#[test]
fn init_already_initialized_human() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("existing-cfg");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(
        target.join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: existing\nspec: {}\n",
    )
    .unwrap();

    let target_str = target.to_string_lossy().into_owned();
    let args = InitArgs {
        path: Some(&target_str),
        from: None,
        branch: "master",
        name: None,
        apply: false,
        dry_run: false,
        yes: true,
        install_daemon: false,
        theme: None,
        apply_profile: None,
        apply_modules: &[],
    };

    let (old_printer, _sink) = cfgd_core::output::Printer::for_test();
    let (printer, cap) = Printer::for_test_doc();
    cmd_init(&old_printer, &printer, &args).unwrap();
    drop(printer);

    let human = cap.human().replace(&target_str, "<TARGET_DIR>");
    let normalized = strip_ansi(&human);
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "init/already_initialized.txt",
        &normalized,
    );
}

#[test]
#[serial_test::serial]
fn init_with_apply_streaming_to_buffered() {
    // Anchor for the streaming → buffered bridge under real apply data. We
    // scaffold a fresh dir + immediately `--apply-profile default --dry-run`
    // against an empty profile whose resolved plan has zero actions, so
    // apply_plan reaches its "Nothing to do" early-return. The capture
    // covers Phase A (scaffold status lines + git-init success), Phase B
    // (apply header + "Set active profile" + "Nothing to do" status), and
    // Phase C (the trailing buffered Doc anchored on InitOutput).
    let tmp = tempfile::tempdir().unwrap();
    let _home = cfgd_core::with_test_home_guard(tmp.path());
    // Redirect the apply-step state store off the shared default DB so this
    // test doesn't contend with other --apply tests under parallel runs.
    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    // SAFETY: serialized via #[serial].
    unsafe {
        std::env::set_var("CFGD_STATE_DIR", &state_dir);
    }
    let target = tmp.path().join("bridge-cfg");
    let target_str = target.to_string_lossy().into_owned();

    // We let scaffold create cfgd.yaml + .gitignore + .github workflow, but
    // need profiles/default.yaml to exist BEFORE --apply-profile runs. The
    // scaffold step creates the profiles/ directory, so we drop the profile
    // file in after `cmd_init` finishes scaffolding but before apply…
    // except cmd_init runs scaffold-then-apply atomically inside a single
    // call. So we pre-create the profiles dir + profile file, then let
    // scaffold's create_dir_all be a no-op for that subdir.
    std::fs::create_dir_all(target.join("profiles")).unwrap();
    std::fs::write(
        target.join("profiles").join("default.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
    )
    .unwrap();

    let args = InitArgs {
        path: Some(&target_str),
        from: None,
        branch: "master",
        name: Some("bridge-cfg"),
        apply: false,
        dry_run: true,
        yes: true,
        install_daemon: false,
        theme: None,
        apply_profile: Some("default"),
        apply_modules: &[],
    };

    let (old_printer, _sink) = cfgd_core::output::Printer::for_test();
    let (printer, cap) = Printer::for_test_doc();
    let result = cmd_init(&old_printer, &printer, &args);
    drop(printer);
    // SAFETY: serialized via #[serial].
    unsafe {
        std::env::remove_var("CFGD_STATE_DIR");
    }
    result.unwrap();

    let human = cap.human().replace(&target_str, "<TARGET_DIR>");
    let normalized = strip_ansi(&human);
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "init/with_apply_streaming_to_buffered.txt",
        &normalized,
    );
}

// ─────────────────────────────────────────────────────
// snapshot helpers — local to keep tests/output_snapshots/ self-contained
// ─────────────────────────────────────────────────────

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
