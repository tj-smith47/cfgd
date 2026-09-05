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
//!     (the real `ApplyRun` header + preview tree `apply_plan` emits for a
//!     non-empty plan) followed by a buffered Doc carrying a real
//!     `section("Next Steps", |s| s.kv_block(...))` payload. Asserts the
//!     one-blank-line bridge rule programmatically.
//!
//! Goldens live under `tests/output_snapshots/init/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test init_snapshots

use std::path::Path;

use cfgd::cli::init::{InitArgs, cmd_init};
use cfgd_core::assert_snapshot_golden as assert_snapshot;
use cfgd_core::output::Printer;

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

#[test]
fn init_happy_human() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("happy-cfg");
    let target_str = target.to_string_lossy().into_owned();
    let args = InitArgs {
        on_conflict: cfgd::cli::OnConflict::Ask,
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
        cache_dir: None,
        state_dir: None,
        runtime_dir: None,
        scope: cfgd_core::Scope::User,
    };

    let (printer, cap) = Printer::for_test_doc();
    cmd_init(&printer, &args).unwrap();
    drop(printer);

    // Replace the tempdir-rooted target with a stable placeholder so the
    // golden survives across hosts (tempfile paths embed `/tmp/.tmpXXXX`).
    // Use `normalize_for_snapshot` so the captured text and substitution
    // key share the posix separator convention on Windows.
    let normalized =
        cfgd_core::normalize_for_snapshot(&strip_ansi(&cap.human()), &[(&target, "<TARGET_DIR>")]);
    assert_snapshot!(Path::new(SNAPSHOT_ROOT), "init/happy.txt", &normalized);
}

/// The `--from` path names its destination ONCE: the clone row is the verdict,
/// and the `Initialized at` row a scaffold closes on would restate it one line
/// down. The source repo is a local checkout so the clone is a real `git
/// clone` with no network.
#[test]
fn init_from_a_local_repo_names_the_destination_once() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("upstream");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(
        source.join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: upstream\nspec: {}\n",
    )
    .unwrap();
    let repo = git2::Repository::init(&source).unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("cfgd.yaml")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        // A fixed signature time, because the clone row now names the commit
        // it landed on: `Signature::now` would mint a different id every run
        // and the golden would be unpinnable.
        let sig = git2::Signature::new(
            "cfgd test",
            "test@cfgd.local",
            &git2::Time::new(1_700_000_000, 0),
        )
        .unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
    }
    let branch = repo.head().unwrap().shorthand().unwrap().to_string();
    let target = tmp.path().join("from-cfg");
    let target_str = target.to_string_lossy().into_owned();
    let source_str = source.to_string_lossy().into_owned();
    let args = InitArgs {
        on_conflict: cfgd::cli::OnConflict::Ask,
        path: Some(&target_str),
        from: Some(&source_str),
        branch: &branch,
        name: None,
        apply: false,
        dry_run: false,
        yes: true,
        install_daemon: false,
        theme: None,
        apply_profile: None,
        apply_modules: &[],
        cache_dir: None,
        state_dir: None,
        runtime_dir: None,
        scope: cfgd_core::Scope::User,
    };

    let (printer, cap) = Printer::for_test_doc();
    cmd_init(&printer, &args).unwrap();
    drop(printer);

    let normalized = cfgd_core::normalize_for_snapshot(
        &strip_ansi(&cap.human()),
        &[(&target, "<TARGET_DIR>"), (&source, "<SOURCE_DIR>")],
    );
    // A local clone is asked for at full depth: `--depth` on a local path
    // makes git stream `warning: --depth is ignored in local clones` under
    // the clone row, a line cfgd cannot word and a reader cannot act on.
    assert!(
        !normalized.lines().any(|l| l.contains("warning:")),
        "a local-path clone must not provoke a git warning:\n{normalized}"
    );
    // cfgd's own rows name the destination once, on the clone row; git's
    // `Cloning into '…'…` passthrough under it is git's line, not a second
    // cfgd row.
    let cfgd_rows = normalized
        .lines()
        .filter(|l| !l.contains("Cloning into"))
        .filter(|l| l.contains("<TARGET_DIR>"))
        .count();
    assert_eq!(
        cfgd_rows, 1,
        "the destination is named once, on the clone row:\n{normalized}"
    );
    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "init/from_local_repo.txt",
        &normalized
    );
}

#[test]
fn init_happy_json() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("happy-cfg-json");
    let target_str = target.to_string_lossy().into_owned();
    let args = InitArgs {
        on_conflict: cfgd::cli::OnConflict::Ask,
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
        cache_dir: None,
        state_dir: None,
        runtime_dir: None,
        scope: cfgd_core::Scope::User,
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
    assert_snapshot!(Path::new(SNAPSHOT_ROOT), "init/happy.json", &actual);
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
        on_conflict: cfgd::cli::OnConflict::Ask,
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
        cache_dir: None,
        state_dir: None,
        runtime_dir: None,
        scope: cfgd_core::Scope::User,
    };

    let (printer, cap) = Printer::for_test_doc();
    cmd_init(&printer, &args).unwrap();
    drop(printer);

    let normalized =
        cfgd_core::normalize_for_snapshot(&strip_ansi(&cap.human()), &[(&target, "<TARGET_DIR>")]);
    assert_snapshot!(
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
    // below — kept separate because exercising it requires a buffered Doc
    // with human content, which cmd_init does not emit on the apply branch.
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

    // Scaffold creates cfgd.yaml + .gitignore + .github workflow, but
    // profiles/default.yaml needs to exist BEFORE --apply-profile runs. The
    // scaffold step creates the profiles/ directory, so the profile
    // file is dropped in after `cmd_init` finishes scaffolding but before apply…
    // except cmd_init runs scaffold-then-apply atomically inside a single
    // call. So this pre-creates the profiles dir + profile file, then lets
    // scaffold's create_dir_all be a no-op for that subdir.
    std::fs::create_dir_all(target.join("profiles")).unwrap();
    std::fs::write(
        target.join("profiles").join("default.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
    )
    .unwrap();

    let args = InitArgs {
        on_conflict: cfgd::cli::OnConflict::Ask,
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
        cache_dir: None,
        state_dir: None,
        runtime_dir: None,
        scope: cfgd_core::Scope::User,
    };

    let (printer, cap) = Printer::for_test_doc();
    let result = cmd_init(&printer, &args);
    drop(printer);
    // SAFETY: serialized via #[serial].
    unsafe {
        std::env::remove_var("CFGD_STATE_DIR");
    }
    result.unwrap();

    let normalized =
        cfgd_core::normalize_for_snapshot(&strip_ansi(&cap.human()), &[(&target, "<TARGET_DIR>")]);
    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "init/with_apply_renders_apply_status_streaming.txt",
        &normalized,
    );
}

#[test]
#[serial_test::serial]
fn init_theme_rethemed_printer_still_owes_apply_a_blank_line() {
    // `cfgd init --theme <preset> --apply-module <m>` re-themes mid-run:
    // `cmd_init` closes its "Initialize cfgd" section (arming blank-pending
    // on the printer it was called with) and then calls `printer.rethemed`,
    // which swaps in a printer whose renderer had never heard of that close.
    // Before `Renderer::with_bars_continued` / `RenderState::continued_from`,
    // the fresh renderer defaulted `leading: true` and dropped the blank
    // line the closed section owed — "Apply" rendered directly under
    // "Initialized at …" whenever `--theme` was passed.
    //
    // Module-only (`apply_profile: None`), not profile-based like the sibling
    // test above: the profile branch prints "Set active profile: …" on the
    // rethemed printer BEFORE the Apply header, and that status line's own
    // group-close re-arms blank-pending independently — masking this exact
    // bug. `cfgd init --theme dracula --apply-module nvim --yes` (the README
    // demo's actual command) takes the module-only branch, which has no such
    // status line between the retheme and "Apply", so it is the one shape
    // that actually exercises the gap this test guards.
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
    let target = tmp.path().join("themed-cfg");
    let target_str = target.to_string_lossy().into_owned();

    // A module with no packages/files plans zero actions, hitting the same
    // "Nothing to do" early return the sibling test relies on — the fix is
    // about header placement, not about executing real package work.
    let module_dir = target.join("modules").join("empty-mod");
    std::fs::create_dir_all(&module_dir).unwrap();
    std::fs::write(
        module_dir.join("module.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: empty-mod\n  description: regression fixture\nspec: {}\n",
    )
    .unwrap();

    let apply_modules = vec!["empty-mod".to_string()];
    let args = InitArgs {
        on_conflict: cfgd::cli::OnConflict::Ask,
        path: Some(&target_str),
        from: None,
        branch: "master",
        name: Some("themed-cfg"),
        apply: false,
        // `false`, not the sibling test's `true`: `RunTitle::as_str()` renders
        // "Plan" under `--dry-run` and "Apply" otherwise, and the real demo
        // command this guards (`cfgd init --theme dracula --apply-module
        // nvim --yes`) never passes `--dry-run` — this has to see the actual
        // "Apply" heading the fix targets, not its preview sibling.
        dry_run: false,
        yes: true,
        install_daemon: false,
        theme: Some("dracula"),
        apply_profile: None,
        apply_modules: &apply_modules,
        cache_dir: None,
        state_dir: None,
        runtime_dir: None,
        scope: cfgd_core::Scope::User,
    };

    let (printer, cap) = Printer::for_test_doc();
    let result = cmd_init(&printer, &args);
    drop(printer);
    // SAFETY: serialized via #[serial].
    unsafe {
        std::env::remove_var("CFGD_STATE_DIR");
    }
    result.unwrap();

    let human = strip_ansi(&cap.human());
    let lines: Vec<&str> = human.lines().collect();
    let apply_line = lines
        .iter()
        .position(|&l| l == "Apply")
        .expect("the re-themed printer's apply section renders an \"Apply\" heading");
    assert_eq!(
        lines.get(apply_line.wrapping_sub(1)),
        Some(&""),
        "expected a blank line directly above the re-themed printer's \"Apply\" heading, got:\n{human}"
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
    // This test fills that gap by driving the same printer with the run
    // skeleton `apply_plan` produces for a non-empty plan under `--dry-run`
    // — the real `ApplyRun` header and preview of a preview-only run, not a
    // hand-written imitation of them —
    // and then emitting a buffered Doc carrying a real
    // `section("Next Steps", |s| s.kv_block(...))` payload. The snapshot pins
    // the rendered output and the assertions below confirm the bridge
    // invariant: exactly one blank line between the last streaming line and
    // the first buffered line.
    use cfgd_core::output::Doc;
    use cfgd_core::providers::FileAction;
    use cfgd_core::reconciler::{Action, ApplyRun, Owner, Phase, PhaseName, Plan, RunContext};

    let tmp = tempfile::tempdir().unwrap();
    let (printer, cap) = cfgd_core::output::Printer::for_test_doc();

    // Streaming portion — the header rows and preview tree `apply_plan`
    // renders for a one-action plan under `--dry-run`.
    let config_path = tmp.path().join("cfgd.yaml");
    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Files,
            &Owner::profile("default"),
            vec![Action::File(FileAction::Create {
                source: tmp.path().join("files/gitconfig"),
                target: tmp.path().join("home/.gitconfig"),
                origin: "local".to_string(),
                strategy: cfgd_core::config::FileStrategy::Symlink,
                source_hash: None,
                patch: None,
            })],
        )],
        warnings: Vec::new(),
    };
    let modules: Vec<cfgd_core::output::HeaderModule> = Vec::new();
    let run = ApplyRun::new(
        RunContext {
            title: cfgd_core::reconciler::RunTitle::Plan,
            config_path: Some(config_path.as_path()),
            profile: Some("default"),
            sources: &[],
            modules: &modules,
            profile_inherits: &[],
            trigger: None,
            subject: None,
            unit_source: None,
        },
        &plan,
    )
    .preview_only();
    run.header(&printer);
    run.preview(&printer);
    printer.status_simple(
        cfgd_core::output::Role::Info,
        format!(
            "{} planned",
            cfgd_core::pluralize(plan.total_actions(), "action")
        ),
    );

    // Buffered portion — a real section with a command_list, matching the
    // shape cmd_init emits when `should_apply == false` (the "Next steps"
    // section in cmd_init.rs).
    let doc = Doc::new().section("Next Steps", |s| {
        s.command_list([
            ("cfgd apply", "apply configuration"),
            ("cfgd status", "view configured state"),
            ("cfgd daemon install", "start background sync"),
        ])
    });
    printer.emit(doc);
    drop(printer);

    let captured =
        cfgd_core::normalize_for_snapshot(&strip_ansi(&cap.human()), &[(tmp.path(), "<TMP>")]);

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

    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "init/apply_then_next_steps.txt",
        &captured,
    );
}

#[test]
#[serial_test::serial]
fn init_apply_lock_honors_state_dir_override() {
    // Regression guard: `cfgd init --apply` must acquire the apply mutex in the
    // dir resolved by `--state-dir` (threaded through `InitArgs.state_dir`), the
    // same dir `cfgd apply` and the daemon lock — otherwise the three fail to
    // mutually-exclude. `acquire_apply_lock(dir)` creates `dir/apply.lock`, which
    // persists after the guard drops, so its presence proves which dir was used.
    let tmp = tempfile::tempdir().unwrap();
    // Sandbox HOME so the UNFIXED code path (which calls `default_state_dir()`,
    // resolving through HOME) cannot touch the real `~/.local/state/cfgd`, and
    // so its lock lands somewhere OTHER than our `state_dir` override.
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let _home_guard = cfgd_core::test_helpers::EnvVarGuard::set("HOME", home.to_str().unwrap());
    // The override must win over CFGD_STATE_DIR too; leave it unset so the only
    // way the lock reaches `state_dir` is via the flag chain under test.
    let _state_env = cfgd_core::test_helpers::EnvVarGuard::unset("CFGD_STATE_DIR");

    let state_dir = tmp.path().join("explicit-state");
    let cache_dir = tmp.path().join("explicit-cache");
    let target = tmp.path().join("locked-cfg");
    let target_str = target.to_string_lossy().into_owned();

    // The profile must carry at least one action: `apply_plan` early-returns on
    // a zero-action plan BEFORE the lock is acquired, so an empty profile would
    // never reach the lock site this test is asserting on. One managed-file copy
    // is the minimal plan that drives the apply past the lock acquisition.
    std::fs::create_dir_all(target.join("files")).unwrap();
    std::fs::write(target.join("files").join("hello.txt"), "hi").unwrap();
    let deployed = tmp.path().join("deployed").join("hello.txt");
    std::fs::create_dir_all(target.join("profiles")).unwrap();
    std::fs::write(
        target.join("profiles").join("default.yaml"),
        format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  inherits: []\n  modules: []\n  files:\n    managed:\n      - source: files/hello.txt\n        target: {}\n        strategy: Copy\n",
            deployed.display()
        ),
    )
    .unwrap();

    let args = InitArgs {
        on_conflict: cfgd::cli::OnConflict::Ask,
        path: Some(&target_str),
        from: None,
        branch: "master",
        name: Some("locked-cfg"),
        apply: true,
        dry_run: false,
        yes: true,
        install_daemon: false,
        theme: None,
        apply_profile: Some("default"),
        apply_modules: &[],
        cache_dir: Some(cache_dir.as_path()),
        state_dir: Some(state_dir.as_path()),
        runtime_dir: None,
        scope: cfgd_core::Scope::User,
    };

    let (printer, _cap) = Printer::for_test_doc();
    cmd_init(&printer, &args).unwrap();
    drop(printer);

    assert!(
        state_dir.join("apply.lock").exists(),
        "init --apply must acquire the lock in the --state-dir override ({}), \
         not the default state dir",
        state_dir.display()
    );
}

// ─────────────────────────────────────────────────────
// snapshot helpers — local to keep tests/output_snapshots/ self-contained
// ─────────────────────────────────────────────────────

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
