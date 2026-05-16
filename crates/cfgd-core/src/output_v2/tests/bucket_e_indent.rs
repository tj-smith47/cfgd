//! Bucket (e): indent invariant. 6 cases — covers the runtime indent check
//! at depths 1/2/3, the §6.4 empty-section collapse rule, suppressed-child
//! collapse, and the buffered-Doc path that never needs the runtime check.

use crate::golden_doc;
use crate::output_v2::Doc;

golden_doc!(bucket_e, depth_one, |p, cap| {
    let s = p.section("X");
    s.bullet("at depth 1");
});

golden_doc!(bucket_e, depth_two, |p, cap| {
    let s = p.section("Outer");
    let n = s.section("Inner");
    n.bullet("at depth 2");
});

golden_doc!(bucket_e, depth_three, |p, cap| {
    let s = p.section("L1");
    let m = s.section("L2");
    let n = m.section("L3");
    n.bullet("deep");
});

golden_doc!(
    bucket_e,
    section_or_collapse_with_no_children_renders_nothing,
    |p, cap| {
        p.heading("Before");
        {
            let _s = p.section_or_collapse("Empty");
        }
        p.heading("After");
    }
);

golden_doc!(
    bucket_e,
    section_with_only_suppressed_hint_collapses,
    |p, cap| {
        // §6.4: verbosity-suppressed components don't count as emitted
        // children. At Normal, Note is Verbose-only, so a section that only
        // emits a Note collapses under `section_or_collapse`.
        let s = p.section_or_collapse("MaybeEmpty");
        s.note("verbose-only — suppressed at Normal");
    }
);

golden_doc!(bucket_e, buffered_doc_no_runtime_check_needed, |p, cap| {
    let doc = Doc::new()
        .heading("Buffered")
        .section("Files", |s| s.bullet("foo"));
    p.emit(doc);
});
