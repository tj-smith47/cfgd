//! Bucket (a): per-component baseline. 9 cases, one per component, default
//! theme, Normal verbosity.

use crate::golden_doc;
use crate::output_v2::Role;
use crate::output_v2::renderer::Table;

golden_doc!(bucket_a, heading_only, |p, cap| {
    p.heading("My Command");
});

golden_doc!(bucket_a, section_with_one_bullet, |p, cap| {
    let s = p.section("Files");
    s.bullet("foo.txt");
});

golden_doc!(bucket_a, kv_block_three_pairs, |p, cap| {
    p.kv_block([("Foo", "1"), ("LongerKey", "2"), ("X", "3")]);
});

golden_doc!(bucket_a, bullet_inside_section, |p, cap| {
    let s = p.section("List");
    s.bullet("only one");
});

golden_doc!(bucket_a, status_ok_simple, |p, cap| {
    p.status_simple(Role::Ok, "wrote /etc/hosts");
});

golden_doc!(bucket_a, hint_at_normal, |p, cap| {
    p.hint("run cfgd apply to apply");
});

golden_doc!(bucket_a, note_suppressed_at_normal, |p, cap| {
    // Note suppressed at Normal — golden is empty.
    p.note("this is a long note that should not appear");
});

golden_doc!(bucket_a, table_two_cols_two_rows, |p, cap| {
    p.table(
        Table::new(["Name", "Age"])
            .row(["alice", "30"])
            .row(["bob", "25"]),
    );
});

golden_doc!(bucket_a, progress_hidden_in_test, |p, cap| {
    // Spinner is hidden when not a TTY (in tests). Golden captures the
    // spinner-Drop's Info Status fallback.
    let _sp = p.spinner("would-be-running");
});
