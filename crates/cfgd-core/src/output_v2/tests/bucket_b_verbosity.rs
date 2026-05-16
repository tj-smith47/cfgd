//! Bucket (b): per-component verbosity sweep. 27 cases — every component
//! crossed with {Quiet, Normal, Verbose} at default theme. Catches the §12
//! verbosity-suppression rules and the "emitted a child" rule in §6.4.

use crate::golden_at;
use crate::output_v2::{Role, Verbosity};

// Heading × {Quiet, Normal, Verbose}
golden_at!(bucket_b, heading_quiet, Verbosity::Quiet, |p| {
    p.heading("X");
});
golden_at!(bucket_b, heading_normal, Verbosity::Normal, |p| {
    p.heading("X");
});
golden_at!(bucket_b, heading_verbose, Verbosity::Verbose, |p| {
    p.heading("X");
});

// Section (with one bullet) × verbosity
golden_at!(bucket_b, section_quiet, Verbosity::Quiet, |p| {
    let s = p.section("X");
    s.bullet("a");
});
golden_at!(bucket_b, section_normal, Verbosity::Normal, |p| {
    let s = p.section("X");
    s.bullet("a");
});
golden_at!(bucket_b, section_verbose, Verbosity::Verbose, |p| {
    let s = p.section("X");
    s.bullet("a");
});

// KvBlock × verbosity
golden_at!(bucket_b, kv_quiet, Verbosity::Quiet, |p| {
    p.kv_block([("k", "v")]);
});
golden_at!(bucket_b, kv_normal, Verbosity::Normal, |p| {
    p.kv_block([("k", "v")]);
});
golden_at!(bucket_b, kv_verbose, Verbosity::Verbose, |p| {
    p.kv_block([("k", "v")]);
});

// Bullet × verbosity (need to be in a section; tests bullet's verbosity gating)
golden_at!(bucket_b, bullet_quiet, Verbosity::Quiet, |p| {
    let s = p.section("X");
    s.bullet("a");
});
golden_at!(bucket_b, bullet_normal, Verbosity::Normal, |p| {
    let s = p.section("X");
    s.bullet("a");
});
golden_at!(bucket_b, bullet_verbose, Verbosity::Verbose, |p| {
    let s = p.section("X");
    s.bullet("a");
});

// Status(Ok) × verbosity (Quiet should suppress, Normal+ should show)
golden_at!(bucket_b, status_ok_quiet, Verbosity::Quiet, |p| {
    p.status_simple(Role::Ok, "ok");
});
golden_at!(bucket_b, status_ok_normal, Verbosity::Normal, |p| {
    p.status_simple(Role::Ok, "ok");
});
golden_at!(bucket_b, status_ok_verbose, Verbosity::Verbose, |p| {
    p.status_simple(Role::Ok, "ok");
});

// Status(Fail) × verbosity (Fail should render even at Quiet)
golden_at!(bucket_b, status_fail_quiet, Verbosity::Quiet, |p| {
    p.status_simple(Role::Fail, "boom");
});
golden_at!(bucket_b, status_fail_normal, Verbosity::Normal, |p| {
    p.status_simple(Role::Fail, "boom");
});
golden_at!(bucket_b, status_fail_verbose, Verbosity::Verbose, |p| {
    p.status_simple(Role::Fail, "boom");
});

// Hint × verbosity (shown at Normal+, suppressed at Quiet)
golden_at!(bucket_b, hint_quiet, Verbosity::Quiet, |p| {
    p.hint("h");
});
golden_at!(bucket_b, hint_normal, Verbosity::Normal, |p| {
    p.hint("h");
});
golden_at!(bucket_b, hint_verbose, Verbosity::Verbose, |p| {
    p.hint("h");
});

// Note × verbosity (only Verbose)
golden_at!(bucket_b, note_quiet, Verbosity::Quiet, |p| {
    p.note("long prose");
});
golden_at!(bucket_b, note_normal, Verbosity::Normal, |p| {
    p.note("long prose");
});
golden_at!(bucket_b, note_verbose, Verbosity::Verbose, |p| {
    p.note("long prose");
});

// Table × verbosity
golden_at!(bucket_b, table_quiet, Verbosity::Quiet, |p| {
    use crate::output_v2::renderer::Table;
    p.table(Table::new(["k"]).row(["v"]));
});
golden_at!(bucket_b, table_normal, Verbosity::Normal, |p| {
    use crate::output_v2::renderer::Table;
    p.table(Table::new(["k"]).row(["v"]));
});
golden_at!(bucket_b, table_verbose, Verbosity::Verbose, |p| {
    use crate::output_v2::renderer::Table;
    p.table(Table::new(["k"]).row(["v"]));
});
