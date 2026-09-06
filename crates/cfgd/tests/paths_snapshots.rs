//! Snapshot tests for `cfgd paths`.
//!
//! Goldens live under `tests/output_snapshots/paths/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test paths_snapshots
//!
//! A live `cmd_paths` resolves the host's own roots, so the goldens drive
//! `build_paths_doc` with the FHS system roots and the XDG user roots written
//! out — the two layouts `docs/configuration.md` shows, captured rather than
//! transcribed.

use std::path::Path;

use cfgd::cli::paths::{
    CachePaths, ConfigPaths, DirSource, PathsOutput, RuntimePaths, StatePaths, build_paths_doc,
};
use cfgd_core::output::Printer;

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

fn user_fixture() -> PathsOutput {
    PathsOutput {
        scope: "user",
        scope_named_by_invocation: false,
        config: ConfigPaths {
            dir: "/home/you/.config/cfgd".into(),
            source: DirSource::Default,
            file: "/home/you/.config/cfgd/cfgd.yaml".into(),
        },
        state: StatePaths {
            dir: Some("/home/you/.local/state/cfgd".into()),
            source: DirSource::Default,
            db: Some("/home/you/.local/state/cfgd/state.db".into()),
            apply_lock: Some("/home/you/.local/state/cfgd/apply.lock".into()),
        },
        cache: CachePaths {
            dir: Some("/home/you/.cache/cfgd".into()),
            source: DirSource::Default,
            sources: Some("/home/you/.cache/cfgd/sources".into()),
            modules: Some("/home/you/.cache/cfgd/modules".into()),
        },
        runtime: RuntimePaths {
            dir: Some("/run/user/1000/cfgd".into()),
            source: DirSource::Default,
            socket: "/run/user/1000/cfgd/cfgd.sock".into(),
        },
    }
}

fn system_fixture() -> PathsOutput {
    PathsOutput {
        scope: "system",
        scope_named_by_invocation: false,
        config: ConfigPaths {
            dir: "/etc/cfgd".into(),
            source: DirSource::Default,
            file: "/etc/cfgd/cfgd.yaml".into(),
        },
        state: StatePaths {
            dir: Some("/var/lib/cfgd".into()),
            source: DirSource::Default,
            db: Some("/var/lib/cfgd/state.db".into()),
            apply_lock: Some("/var/lib/cfgd/apply.lock".into()),
        },
        cache: CachePaths {
            dir: Some("/var/cache/cfgd".into()),
            source: DirSource::Default,
            sources: Some("/var/cache/cfgd/sources".into()),
            modules: Some("/var/cache/cfgd/modules".into()),
        },
        runtime: RuntimePaths {
            dir: Some("/run/cfgd".into()),
            source: DirSource::Default,
            socket: "/run/cfgd/cfgd.sock".into(),
        },
    }
}

/// A root the host could not resolve renders its marker rather than an empty
/// value column, which reads as a path of length zero.
fn homeless_fixture() -> PathsOutput {
    let mut output = user_fixture();
    output.state.dir = None;
    output.state.db = None;
    output.state.apply_lock = None;
    output
}

#[test]
fn paths_user_scope_human() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_paths_doc(&user_fixture()));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "paths/user.txt");
}

#[test]
fn paths_user_scope_json() {
    let output = user_fixture();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_paths_doc(&output));
    drop(printer);
    let expected = serde_json::to_value(&output).expect("payload serializes");
    let actual = cap.json().expect("doc captured json");
    pretty_assertions::assert_eq!(
        actual,
        expected,
        "`paths -o json` is exactly PathsOutput: the human labels are Title \
         Case, the payload keys stay the camelCase a script reads"
    );
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "paths/user.json");
}

/// A scoped command never echoes its invocation-named scope back as an
/// annotation: `cfgd --scope system paths` renders no `Scope` row, because the
/// reader wrote the word. A defaulted scope is the only thing that can say
/// which family these roots belong to, so its row stays.
///
/// The payload is unconditional either way: a scripting consumer never saw the
/// command line.
#[test]
fn a_scope_the_invocation_named_is_not_restated_as_a_row() {
    let render = |named: bool| {
        let mut output = system_fixture();
        output.scope_named_by_invocation = named;
        let (printer, cap) = Printer::for_test_doc();
        printer.emit(build_paths_doc(&output));
        drop(printer);
        (
            cap.human(),
            serde_json::to_value(&output).expect("payload serializes"),
        )
    };

    let (defaulted, defaulted_json) = render(false);
    assert!(
        defaulted.contains("Scope  system"),
        "a defaulted scope names itself: {defaulted}"
    );
    let (flagged, flagged_json) = render(true);
    assert!(
        !flagged.contains("Scope"),
        "`--scope system` is not read back to the reader: {flagged}"
    );
    assert_eq!(
        defaulted_json, flagged_json,
        "the row is display-only: the payload carries the scope either way"
    );
}

#[test]
fn paths_system_scope_human() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_paths_doc(&system_fixture()));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "paths/system.txt");
}

#[test]
fn paths_unresolvable_state_root_human() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_paths_doc(&homeless_fixture()));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "paths/no_home.txt");
}
