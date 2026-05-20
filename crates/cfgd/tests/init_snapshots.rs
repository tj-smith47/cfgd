//! Snapshot tests for `cfgd init`.
//!
//! Four cases:
//!   - `init/happy.{txt,json}` — fresh scaffold to a new directory, no
//!     `--apply`. Exercises the streaming Phase A (scaffold status lines +
//!     git-init success) and the buffered Phase C "Next steps" Doc, so the
//!     streaming → buffered bridge holds its one-blank-line invariant under
//!     real init data.
//!   - `init/already_initialized.txt` — target dir already has `cfgd.yaml`;
//!     pins the short-circuit status line + the buffered "no-section" Doc
//!     emit path.
//!   - `init/with_apply_renders_apply_status_streaming.txt` — `--apply
//!     --dry-run` against an empty profile (plan has zero actions, so
//!     `apply_plan` early-returns on "Nothing to do"). When `should_apply ==
//!     true`, cmd_init suppresses the trailing "Next steps" section and the
//!     final `printer.emit(...)` carries only the typed payload — no
//!     buffered human content. This snapshot therefore pins the apply-status
//!     streaming surface end-to-end, NOT a streaming → buffered human
//!     transition. The bridge invariant under apply data is asserted by the
//!     companion `init_apply_then_next_steps_bridge_invariant` test below.
//!   - `init/apply_then_next_steps.txt` — bridge anchor: a streaming portion
//!     (heading + status lines mirroring the shape `apply_plan` emits for a
//!     non-empty plan) followed by a buffered Doc carrying a real
//!     `section("Next Steps", |s| s.bullet(...))` payload. Asserts the
//!     one-blank-line bridge rule programmatically.
//!
//! Goldens live under `tests/output_snapshots/init/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test init_snapshots

use std::path::Path;

use cfgd::cli::init::{InitArgs, cmd_init};
use cfgd_core::output::Printer;

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

    let (printer, cap) = Printer::for_test_doc();
    cmd_init(&printer, &args).unwrap();
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

    let (printer, cap) = Printer::for_test_doc();
    cmd_init(&printer, &args).unwrap();
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

    let (printer, cap) = Printer::for_test_doc();
    cmd_init(&printer, &args).unwrap();
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
fn init_with_apply_renders_apply_status_streaming() {
    // Pins the apply-status streaming surface of `cfgd init --apply
    // --dry-run` against an empty profile (zero actions, so `apply_plan`
    // hits its "Nothing to do" early-return). When `should_apply == true`
    // cmd_init suppresses the "Next steps" section in its trailing
    // `printer.emit(...)`, so the final Doc carries only the InitOutput
    // payload — NOT a buffered human surface. This capture therefore covers
    // the scaffold surface (scaffold status lines + git-init success) and
    // the apply surface (apply header + "Set active profile" + "Nothing to
    // do" status), with no buffered human content trailing it. The
    // streaming → buffered one-blank-line invariant under apply data is
    // asserted by the `init_apply_then_next_steps_bridge_invariant` test
    // below — kept
    // separate because exercising it requires a buffered Doc with human
    // content, which cmd_init does not emit on the apply branch.
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

    let (printer, cap) = Printer::for_test_doc();
    let result = cmd_init(&printer, &args);
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
        "init/with_apply_renders_apply_status_streaming.txt",
        &normalized,
    );
}

#[test]
fn init_apply_then_next_steps_bridge_invariant() {
    // Bridge anchor: cmd_init's apply branch deliberately suppresses the
    // "Next steps" buffered section (the apply path already produced its
    // own report), so the trailing `printer.emit(...)` there carries
    // only a payload-bearing Doc with no human content — meaning cmd_init
    // alone does NOT exercise a streaming → buffered human transition
    // under apply data.
    //
    // This test fills that gap by driving the same printer with the
    // shape `apply_plan` produces for a non-empty plan (heading +
    // status_simple lines) and then emitting a buffered Doc carrying a
    // real `section("Next Steps", |s| s.bullet(...))` payload. The
    // snapshot pins the rendered output and the assertions below confirm
    // the bridge invariant: exactly one blank line between the last
    // streaming line and the first buffered line.
    use cfgd_core::output::{Doc, Role};

    let (printer, cap) = cfgd_core::output::Printer::for_test_doc();

    // Streaming portion — mirrors `apply_plan`'s shape for a non-empty
    // plan: heading + plan-table section + "N action(s) planned" info
    // line. We use status_simple rather than restanding the plan-table
    // renderer because the bridge invariant is about the gap between
    // streaming and buffered, not the plan-table interior.
    printer.heading("Applying Configuration");
    printer.status_simple(Role::Ok, "Set active profile: default");
    printer.status_simple(Role::Info, "1 action(s) planned");

    // Buffered portion — a real section with bullets, matching the shape
    // cmd_init emits when `should_apply == false` (the "Next steps"
    // section in cmd_init.rs).
    let doc = Doc::new().section("Next Steps", |s| {
        s.bullet("cfgd apply           — apply configuration")
            .bullet("cfgd status         — view configured state")
            .bullet("cfgd daemon install — start background sync")
    });
    printer.emit(doc);
    drop(printer);

    let captured = strip_ansi(&cap.human());

    // Bridge invariant: exactly one blank line between the streaming
    // surface's last line and the buffered Doc's first line. Two newlines
    // in a row means one blank line; three or more means more than one.
    assert!(
        captured.contains("\n\n"),
        "expected at least one blank line between streaming and buffered, got:\n{captured}"
    );
    assert!(
        !captured.contains("\n\n\n"),
        "expected at most one blank line between streaming and buffered, got:\n{captured}"
    );

    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "init/apply_then_next_steps.txt",
        &captured,
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
