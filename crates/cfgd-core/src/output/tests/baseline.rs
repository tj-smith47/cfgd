//! Baseline rendering: per-component baseline. 9 cases, one per component, default
//! theme, Normal verbosity.

use crate::golden_doc;
use crate::output::Role;
use crate::output::renderer::Table;

golden_doc!(baseline, heading_only, |p, cap| {
    p.heading("My Command");
});

golden_doc!(baseline, section_with_one_bullet, |p, cap| {
    let s = p.section("Files");
    s.bullet("foo.txt");
});

golden_doc!(baseline, kv_block_three_pairs, |p, cap| {
    p.kv_block([("Foo", "1"), ("LongerKey", "2"), ("X", "3")]);
});

golden_doc!(baseline, bullet_inside_section, |p, cap| {
    let s = p.section("List");
    s.bullet("only one");
});

golden_doc!(baseline, status_ok_simple, |p, cap| {
    p.status_simple(Role::Ok, "wrote /etc/hosts");
});

golden_doc!(baseline, hint_at_normal, |p, cap| {
    p.hint("run cfgd apply to apply");
});

golden_doc!(baseline, note_suppressed_at_normal, |p, cap| {
    // Note suppressed at Normal — golden is empty.
    p.note("this is a long note that should not appear");
});

golden_doc!(baseline, table_two_cols_two_rows, |p, cap| {
    p.table(
        Table::new(["Name", "Age"])
            .row(["alice", "30"])
            .row(["bob", "25"]),
    );
});

golden_doc!(baseline, progress_hidden_in_test, |p, cap| {
    // Spinner is hidden when not a TTY (in tests). Golden captures the
    // spinner-Drop's Info Status fallback.
    let _sp = p.spinner("would-be-running");
});
