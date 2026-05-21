//! Verbosity sweep: per-component verbosity sweep. 27 cases — every component
//! crossed with {Quiet, Normal, Verbose} at default theme. Catches the
//! verbosity-suppression rules and the "emitted a child" rule.

use crate::golden_at;
use crate::output::{Printer, Role, Verbosity};

// Heading × {Quiet, Normal, Verbose}
golden_at!(verbosity, heading_quiet, Verbosity::Quiet, |p| {
    p.heading("X");
});
golden_at!(verbosity, heading_normal, Verbosity::Normal, |p| {
    p.heading("X");
});
golden_at!(verbosity, heading_verbose, Verbosity::Verbose, |p| {
    p.heading("X");
});

// Section (with one bullet) × verbosity
golden_at!(verbosity, section_quiet, Verbosity::Quiet, |p| {
    let s = p.section("X");
    s.bullet("a");
});
golden_at!(verbosity, section_normal, Verbosity::Normal, |p| {
    let s = p.section("X");
    s.bullet("a");
});
golden_at!(verbosity, section_verbose, Verbosity::Verbose, |p| {
    let s = p.section("X");
    s.bullet("a");
});

// KvBlock × verbosity
golden_at!(verbosity, kv_quiet, Verbosity::Quiet, |p| {
    p.kv_block([("k", "v")]);
});
golden_at!(verbosity, kv_normal, Verbosity::Normal, |p| {
    p.kv_block([("k", "v")]);
});
golden_at!(verbosity, kv_verbose, Verbosity::Verbose, |p| {
    p.kv_block([("k", "v")]);
});

// Bullet × verbosity (need to be in a section; tests bullet's verbosity gating)
golden_at!(verbosity, bullet_quiet, Verbosity::Quiet, |p| {
    let s = p.section("X");
    s.bullet("a");
});
golden_at!(verbosity, bullet_normal, Verbosity::Normal, |p| {
    let s = p.section("X");
    s.bullet("a");
});
golden_at!(verbosity, bullet_verbose, Verbosity::Verbose, |p| {
    let s = p.section("X");
    s.bullet("a");
});

// Status(Ok) × verbosity (Quiet should suppress, Normal+ should show)
golden_at!(verbosity, status_ok_quiet, Verbosity::Quiet, |p| {
    p.status_simple(Role::Ok, "ok");
});
golden_at!(verbosity, status_ok_normal, Verbosity::Normal, |p| {
    p.status_simple(Role::Ok, "ok");
});
golden_at!(verbosity, status_ok_verbose, Verbosity::Verbose, |p| {
    p.status_simple(Role::Ok, "ok");
});

// Status(Fail) × verbosity (Fail should render even at Quiet)
golden_at!(verbosity, status_fail_quiet, Verbosity::Quiet, |p| {
    p.status_simple(Role::Fail, "boom");
});
golden_at!(verbosity, status_fail_normal, Verbosity::Normal, |p| {
    p.status_simple(Role::Fail, "boom");
});
golden_at!(verbosity, status_fail_verbose, Verbosity::Verbose, |p| {
    p.status_simple(Role::Fail, "boom");
});

// Hint × verbosity (shown at Normal+, suppressed at Quiet)
golden_at!(verbosity, hint_quiet, Verbosity::Quiet, |p| {
    p.hint("h");
});
golden_at!(verbosity, hint_normal, Verbosity::Normal, |p| {
    p.hint("h");
});
golden_at!(verbosity, hint_verbose, Verbosity::Verbose, |p| {
    p.hint("h");
});

// Note × verbosity (only Verbose)
golden_at!(verbosity, note_quiet, Verbosity::Quiet, |p| {
    p.note("long prose");
});
golden_at!(verbosity, note_normal, Verbosity::Normal, |p| {
    p.note("long prose");
});
golden_at!(verbosity, note_verbose, Verbosity::Verbose, |p| {
    p.note("long prose");
});

// Table × verbosity
golden_at!(verbosity, table_quiet, Verbosity::Quiet, |p| {
    use crate::output::renderer::Table;
    p.table(Table::new(["k"]).row(["v"]));
});
golden_at!(verbosity, table_normal, Verbosity::Normal, |p| {
    use crate::output::renderer::Table;
    p.table(Table::new(["k"]).row(["v"]));
});
golden_at!(verbosity, table_verbose, Verbosity::Verbose, |p| {
    use crate::output::renderer::Table;
    p.table(Table::new(["k"]).row(["v"]));
});

/// Per `.claude/rules/output-module.md`: `Role::Accent` and `Role::Secondary`
/// are both suppressed at `Verbosity::Quiet` like every non-`Fail` role.
/// Direct assertion (not a golden) so the suppression contract is named and
/// the failure mode is a one-line message instead of a snapshot diff.
#[test]
fn accent_and_secondary_suppressed_at_quiet() {
    let (p, buf) = Printer::for_test_at(Verbosity::Quiet);
    p.status_simple(Role::Accent, "accent line");
    p.status_simple(Role::Secondary, "secondary line");
    p.status_simple(Role::Fail, "fail line");
    p.flush();
    let raw = buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let out = crate::output::strip_ansi(&raw);
    assert!(
        !out.contains("accent line"),
        "accent should be suppressed at Quiet: {out:?}"
    );
    assert!(
        !out.contains("secondary line"),
        "secondary should be suppressed at Quiet: {out:?}"
    );
    assert!(
        out.contains("fail line"),
        "fail should NOT be suppressed at Quiet: {out:?}"
    );
}
