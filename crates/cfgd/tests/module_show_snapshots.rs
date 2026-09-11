//! Snapshot tests for `cfgd module list` and `cfgd module show`.
//!
//! Six cases:
//!   - `module_list/happy.{txt,json}` — populated entries + wide=false table
//!   - `module_list/empty.txt`        — empty list (status + hint shape)
//!   - `module_show/happy.{txt,json}` — populated module with remote lock,
//!     state, packages (mix of Resolved/Skipped/Unresolved), files, env,
//!     aliases, lifecycle scripts
//!   - `module_show/not_found.txt`    — error path (status + hint)
//!
//! Goldens live under `tests/output_snapshots/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test module_show_snapshots

use std::path::Path;

use cfgd::cli::InventoryDetail;
use cfgd::cli::error::render_cli_error;
use cfgd::cli::module::list_show::{
    PackageDisplay, build_module_list_doc, build_module_not_found_error, build_module_show_doc,
};
use cfgd::cli::module::{ModuleListEntry, ModuleShowMetadata, ModuleShowOutput};
use cfgd_core::config::{
    EnvVar, ModuleFileEntry, ModuleLockEntry, ModuleSpec, ScriptCommand, ScriptEntry, ScriptSpec,
    ShellAlias,
};
use cfgd_core::output::{Printer, ScriptsForm, Theme, Verbosity};
use cfgd_core::state::ModuleStateRecord;
use pretty_assertions::assert_eq;

/// Two hours after the fixture's `installed_at`, so a rendered `Last Applied`
/// age reads a fixed `2h ago` in the goldens below.
const NOW: &str = "2026-05-14T12:00:00Z";

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

fn happy_entries() -> Vec<ModuleListEntry> {
    vec![
        ModuleListEntry {
            name: "base".into(),
            active: true,
            source: "local".into(),
            status: cfgd_core::state::MODULE_STATUS_INSTALLED.into(),
            checked: true,
            packages: 3,
            files: 5,
            depends: 0,
        },
        ModuleListEntry {
            name: "dev-tools".into(),
            active: true,
            source: "remote".into(),
            status: "pending".into(),
            checked: true,
            packages: 7,
            files: 2,
            depends: 1,
        },
        ModuleListEntry {
            name: "extras".into(),
            active: false,
            source: "local".into(),
            status: "available".into(),
            checked: true,
            packages: 1,
            files: 0,
            depends: 0,
        },
    ]
}

fn happy_show_output() -> ModuleShowOutput {
    ModuleShowOutput {
        name: "dev-tools".into(),
        metadata: ModuleShowMetadata {
            version: Some("1.4.0".into()),
        },
        directory: "/etc/cfgd/modules/dev-tools".into(),
        source: "remote".into(),
        depends: vec!["base".into()],
        state: Some(ModuleStateRecord {
            module_name: "dev-tools".into(),
            installed_at: "2026-05-14T10:00:00Z".into(),
            last_applied: Some(1_715_680_800),
            packages_hash: "abc123def456".into(),
            files_hash: "789ghi012jkl".into(),
            git_sources: None,
            status: cfgd_core::state::MODULE_STATUS_INSTALLED.into(),
        }),
        spec: ModuleSpec {
            depends: vec!["base".into()],
            platforms: vec![],
            packages: vec![],
            files: vec![
                ModuleFileEntry {
                    patch: None,
                    source: "vimrc".into(),
                    target: "~/.vimrc".into(),
                    strategy: None,
                    private: false,
                    encryption: None,
                    permissions: None,
                },
                ModuleFileEntry {
                    patch: None,
                    source: "git@example.com:org/dotfiles.git//tmux.conf".into(),
                    target: "~/.tmux.conf".into(),
                    strategy: None,
                    private: false,
                    encryption: None,
                    permissions: None,
                },
            ],
            env: vec![
                EnvVar {
                    name: "EDITOR".into(),
                    value: "nvim".into(),
                    platforms: vec![],
                },
                EnvVar {
                    name: "GH_TOKEN".into(),
                    value: "ghp_secret_token_value".into(),
                    platforms: vec![],
                },
            ],
            aliases: vec![
                ShellAlias {
                    name: "gs".into(),
                    command: "git status".into(),
                    platforms: vec![],
                },
                ShellAlias {
                    name: "ll".into(),
                    command: "ls -la".into(),
                    platforms: vec![],
                },
            ],
            // Declared out of run order on purpose: the Scripts section
            // reports the order the hooks RUN in, and every declaring hook —
            // not just `postApply` — has to appear.
            scripts: Some(ScriptSpec {
                post_apply: vec![
                    ScriptEntry::Simple("echo 'post-apply hook ran'".into()),
                    ScriptEntry::Simple("systemctl --user daemon-reload".into()),
                ],
                pre_apply: vec![ScriptEntry::Simple("mkdir -p ~/.config/dev-tools".into())],
                on_drift: vec![ScriptEntry::Simple(
                    "notify-send 'dev-tools drifted'".into(),
                )],
                ..Default::default()
            }),
            system: Default::default(),
        },
    }
}

fn happy_lock_entry() -> ModuleLockEntry {
    ModuleLockEntry {
        name: "dev-tools".into(),
        url: "git@example.com:cfgd/modules.git".into(),
        pinned_ref: "v1.2.3".into(),
        commit: "deadbeef1234567890abcdef1234567890abcdef".into(),
        integrity: "sha256:cafef00d".into(),
        subdir: Some("modules/dev-tools".into()),
    }
}

fn happy_packages() -> Vec<PackageDisplay> {
    vec![
        PackageDisplay::Resolved {
            name: "ripgrep".into(),
            manager: "brew".into(),
            resolved_name: "ripgrep".into(),
            version: Some("14.1.0".into()),
        },
        PackageDisplay::Skipped {
            name: "winget-only-tool".into(),
            platforms: ", platforms: windows".into(),
        },
        PackageDisplay::Unresolved {
            summary: "obscure-tool (prefer: nix), min: 1.0".into(),
            error: "no manager available on this platform".into(),
        },
    ]
}

/// Every hook that declares something gets a row, labelled with the hook it
/// runs in and ordered by when it runs — the section used to render `postApply`
/// bodies alone, so a module's `preApply` and `onDrift` scripts were invisible.
///
/// No drift engine ever watches a hook body, so every row is a bare
/// declaration through `command_list` (hook name as the key, `" — "` glue) —
/// never a `status` row wearing a role glyph (`◉`/`✓`/…) for a check that
/// never ran, the same doctrine `cfgd status <module>`'s Scripts section
/// codifies (`no_declared_inventory_row_wears_a_verdict_glyph`).
#[test]
fn module_show_renders_every_declaring_hook_in_execution_order() {
    let output = happy_show_output();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_module_show_doc(
        &output,
        None,
        &[],
        InventoryDetail {
            values: false,
            scripts: ScriptsForm::Condensed,
        },
        true,
        printer.arrow(),
        NOW,
    ));
    drop(printer);
    let human = cap.human();
    let rows: Vec<String> = human
        .lines()
        .skip_while(|l| l.trim() != "Scripts")
        .skip(1)
        .take_while(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_string())
        .collect();
    assert_eq!(
        rows,
        vec![
            "preApply (1)",
            "mkdir -p ~/.config/dev-tools",
            "postApply (2)",
            "echo 'post-apply hook ran'",
            "systemctl --user daemon-reload",
            "onDrift (1)",
            "notify-send 'dev-tools drifted'",
        ],
        "every declaring hook, in execution order, each step under its own hook: {human}"
    );
    assert!(
        !rows.iter().any(|r| r.contains('◉') || r.contains('✓')),
        "a hook body has no check standing behind it and must not wear a verdict glyph: {rows:?}"
    );
}

/// A module whose `postApply` steps declare the three knobs the full Scripts
/// form states above each body, plus one bare-string step, which states its
/// position alone.
fn knobbed_show_output() -> ModuleShowOutput {
    let mut output = happy_show_output();
    output.spec.scripts = Some(ScriptSpec {
        pre_apply: vec![ScriptEntry::Simple("mkdir -p ~/.config/dev-tools".into())],
        post_apply: vec![
            ScriptEntry::Full(ScriptCommand {
                run: "if command -v pipx >/dev/null 2>&1; then\n  pipx install --force pynvim\nfi"
                    .into(),
                timeout: Some("120s".into()),
                idle_timeout: None,
                continue_on_error: Some(true),
                ..ScriptCommand::default()
            }),
            ScriptEntry::Full(ScriptCommand {
                run: "nvim --headless \"+Lazy! restore\" +qa!".into(),
                timeout: Some("900s".into()),
                idle_timeout: Some("30s".into()),
                continue_on_error: Some(false),
                ..ScriptCommand::default()
            }),
            ScriptEntry::Simple("echo done".into()),
        ],
        ..Default::default()
    });
    output
}

fn emit_knobbed_show(form: ScriptsForm, golden: &str) {
    let output = knobbed_show_output();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_module_show_doc(
        &output,
        None,
        &[],
        InventoryDetail {
            values: false,
            scripts: form,
        },
        true,
        printer.arrow(),
        NOW,
    ));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), golden);
}

/// The default form: one row per step carrying its first line, whatever knobs
/// the step declares — they have no home in a one-line row.
#[test]
fn module_show_scripts_condensed_human() {
    emit_knobbed_show(ScriptsForm::Condensed, "module_show/scripts_condensed.txt");
}

/// `-s` / `-a`: each step states its position and the knobs it declares, then
/// its whole body. The bare-string step states its position alone, and a
/// declared `continueOnError: false` is the default, so no marker names it.
#[test]
fn module_show_scripts_full_human() {
    emit_knobbed_show(ScriptsForm::Full, "module_show/scripts_full.txt");
}

/// The bytes the approved pitch settled, from the real renderer: the Scripts
/// heading, the hook heading with its muted count, the first step's muted
/// marker line and the first highlighted row of its body (panel 1, lines
/// 63-66 of `pitch-nvim-out.txt`). Colour off, these four lines say nothing
/// about the coat each span carries.
#[test]
fn module_show_scripts_full_renders_the_approved_dracula_bytes() {
    let output = pitch_show_output();
    let (printer, buf) = Printer::for_test_with_theme_colored(
        Theme::preset("dracula").expect("dracula is a registered preset"),
        Verbosity::Normal,
    );
    printer.emit(build_module_show_doc(
        &output,
        None,
        &[],
        InventoryDetail {
            values: false,
            scripts: ScriptsForm::Full,
        },
        true,
        printer.arrow(),
        NOW,
    ));
    drop(printer);
    // raw-capture-ok: the pitch's own bytes are the expectation — captured_text would strip exactly what this test compares
    let out = buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let rendered: Vec<&str> = out
        .lines()
        .skip_while(|l| !l.contains("Scripts"))
        .take(4)
        .collect();
    assert_eq!(rendered, PITCH_SCRIPTS_LINES, "the approved bytes: {out:?}");
}

/// The four lines of the approved pitch these tests pin (panel 1, lines
/// 63-66), byte for byte less one zero-width span: the pitch's body line
/// closes on `\x1b[38;2;248;248;242m` before its reset, which is syntect
/// styling the line's own newline. The renderer highlights each line without
/// its terminator, so it emits no escape for a span holding no text.
const PITCH_SCRIPTS_LINES: [&str; 4] = [
    "\x1b[38;2;189;147;249mScripts\x1b[0m",
    "  \x1b[38;2;255;121;198mpostApply\x1b[0m\x1b[38;2;98;114;164m (7)\x1b[0m",
    "    \x1b[38;2;98;114;164m1/7 \u{b7} timeout 120s \u{b7} continueOnError\x1b[0m",
    "    \x1b[38;2;255;121;198mif\x1b[38;2;248;248;242m \x1b[38;2;139;233;253mcommand\x1b[38;2;248;248;242m \x1b[38;2;255;184;108m-\x1b[38;2;255;184;108mv\x1b[38;2;248;248;242m pipx \x1b[38;2;255;121;198m>\x1b[38;2;248;248;242m/dev/null \x1b[38;2;189;147;249m2\x1b[38;2;255;121;198m>&\x1b[38;2;189;147;249m1\x1b[38;2;255;121;198m;\x1b[38;2;248;248;242m \x1b[38;2;255;121;198mthen\x1b[0m",
];

/// The module the pitch was captured from, as far as those four lines reach:
/// one `postApply` hook of seven steps whose first declares `timeout: 120s`
/// and `continueOnError`. The six steps after it carry the count the hook
/// heading states.
fn pitch_show_output() -> ModuleShowOutput {
    let mut output = happy_show_output();
    let mut post_apply = vec![ScriptEntry::Full(ScriptCommand {
        run: "if command -v pipx >/dev/null 2>&1; then\n  pipx install --force pynvim 2>&1 | tail -5 || true\nfi".into(),
        timeout: Some("120s".into()),
        idle_timeout: None,
        continue_on_error: Some(true),
        ..ScriptCommand::default()
    })];
    post_apply.extend((2..=7).map(|n| ScriptEntry::Simple(format!("echo step {n}"))));
    output.spec.scripts = Some(ScriptSpec {
        post_apply,
        ..Default::default()
    });
    output
}

#[test]
fn module_list_happy_human() {
    let entries = happy_entries();
    let config_dir = Path::new("/etc/cfgd");
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_module_list_doc(&entries, false, config_dir));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "module_list/happy.txt");
}

#[test]
fn module_list_happy_json() {
    let entries = happy_entries();
    let config_dir = Path::new("/etc/cfgd");
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_module_list_doc(&entries, false, config_dir));
    drop(printer);
    let expected = serde_json::to_value(&entries).unwrap();
    let actual = cap.json().expect("doc captured json");
    assert_eq!(
        actual, expected,
        "emit -o json must match serde_json::to_value(entries)"
    );
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "module_list/happy.json");
}

#[test]
fn module_list_empty_human() {
    let config_dir = Path::new("/etc/cfgd");
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_module_list_doc(&[], false, config_dir));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "module_list/empty.txt");
}

#[test]
fn module_show_happy_human() {
    let output = happy_show_output();
    let lock = happy_lock_entry();
    let pkgs = happy_packages();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_module_show_doc(
        &output,
        Some(&lock),
        &pkgs,
        InventoryDetail {
            values: false,
            scripts: ScriptsForm::Condensed,
        },
        true,
        printer.arrow(),
        NOW,
    ));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "module_show/happy.txt");
}

#[test]
fn module_show_happy_json() {
    let output = happy_show_output();
    let lock = happy_lock_entry();
    let pkgs = happy_packages();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_module_show_doc(
        &output,
        Some(&lock),
        &pkgs,
        InventoryDetail {
            values: false,
            scripts: ScriptsForm::Condensed,
        },
        true,
        printer.arrow(),
        NOW,
    ));
    drop(printer);
    let expected = serde_json::to_value(&output).unwrap();
    let actual = cap.json().expect("doc captured json");
    assert_eq!(
        actual, expected,
        "emit -o json must match serde_json::to_value(output)"
    );
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "module_show/happy.json");
}

#[test]
fn module_show_not_found_human() {
    // `module show` of a missing module returns a not-found error; the central sink
    // (render_cli_error) renders the one ✗ line + the "Available modules" hint. Drive
    // the real sink so this golden pins exactly what a user sees on the failure path.
    let available: Vec<String> = vec!["base".into(), "dev-tools".into(), "extras".into()];
    let (printer, cap) = Printer::for_test_doc();
    let err = build_module_not_found_error("missing", &available);
    render_cli_error(&printer, &err);
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "module_show/not_found.txt");
}
