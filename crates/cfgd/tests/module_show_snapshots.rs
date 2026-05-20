//! Snapshot tests for `cfgd module list` and `cfgd module show`.
//!
//! Six cases:
//!   - `module_list/happy.{txt,json}` — populated entries + wide=false table
//!   - `module_list/empty.txt`        — empty list (status + hint shape)
//!   - `module_show/happy.{txt,json}` — populated module with remote lock,
//!     state, packages (mix of Resolved/Skipped/Unresolved), files, env,
//!     aliases, post-apply scripts
//!   - `module_show/not_found.txt`    — error path (status + hint)
//!
//! Goldens live under `tests/output_snapshots/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test module_show_snapshots

use std::path::Path;

use cfgd::cli::module::list_show::{
    PackageDisplay, build_module_list_doc, build_module_not_found_doc, build_module_show_doc,
};
use cfgd::cli::module::{ModuleListEntry, ModuleShowOutput};
use cfgd_core::config::{EnvVar, ModuleFileEntry, ModuleLockEntry, ModuleSpec, ShellAlias};
use cfgd_core::output::Printer;
use cfgd_core::state::ModuleStateRecord;
use pretty_assertions::assert_eq;

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

fn happy_entries() -> Vec<ModuleListEntry> {
    vec![
        ModuleListEntry {
            name: "base".into(),
            active: true,
            source: "local".into(),
            status: "applied".into(),
            packages: 3,
            files: 5,
            depends: 0,
        },
        ModuleListEntry {
            name: "dev-tools".into(),
            active: true,
            source: "remote".into(),
            status: "pending".into(),
            packages: 7,
            files: 2,
            depends: 1,
        },
        ModuleListEntry {
            name: "extras".into(),
            active: false,
            source: "local".into(),
            status: "available".into(),
            packages: 1,
            files: 0,
            depends: 0,
        },
    ]
}

fn happy_show_output() -> ModuleShowOutput {
    ModuleShowOutput {
        name: "dev-tools".into(),
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
            status: "applied".into(),
        }),
        spec: ModuleSpec {
            depends: vec!["base".into()],
            packages: vec![],
            files: vec![
                ModuleFileEntry {
                    source: "vimrc".into(),
                    target: "~/.vimrc".into(),
                    strategy: None,
                    private: false,
                    encryption: None,
                },
                ModuleFileEntry {
                    source: "git@example.com:org/dotfiles.git//tmux.conf".into(),
                    target: "~/.tmux.conf".into(),
                    strategy: None,
                    private: false,
                    encryption: None,
                },
            ],
            env: vec![
                EnvVar {
                    name: "EDITOR".into(),
                    value: "nvim".into(),
                },
                EnvVar {
                    name: "GH_TOKEN".into(),
                    value: "ghp_secret_token_value".into(),
                },
            ],
            aliases: vec![
                ShellAlias {
                    name: "gs".into(),
                    command: "git status".into(),
                },
                ShellAlias {
                    name: "ll".into(),
                    command: "ls -la".into(),
                },
            ],
            scripts: None,
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

fn happy_post_apply() -> Vec<String> {
    vec![
        "echo 'post-apply hook ran'".into(),
        "systemctl --user daemon-reload".into(),
    ]
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
    let post = happy_post_apply();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_module_show_doc(
        &output,
        Some(&lock),
        &pkgs,
        &post,
        false,
    ));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "module_show/happy.txt");
}

#[test]
fn module_show_happy_json() {
    let output = happy_show_output();
    let lock = happy_lock_entry();
    let pkgs = happy_packages();
    let post = happy_post_apply();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_module_show_doc(
        &output,
        Some(&lock),
        &pkgs,
        &post,
        false,
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
    let available: Vec<String> = vec!["base".into(), "dev-tools".into(), "extras".into()];
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_module_not_found_doc("missing", &available));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "module_show/not_found.txt");
}
