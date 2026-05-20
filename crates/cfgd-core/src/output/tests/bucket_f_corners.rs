//! Bucket (f): layout corner cases. 12 cases — covers KvBlock alignment at
//! 1/2/many keys, long-key wrap, consecutive `kv` auto-batching,
//! spinner→Status detail/duration chain, 3-level nesting, empty plain
//! section's `(none)` placeholder, `section_if_nonempty` skip, `with_data`
//! divergence, status target path, and hint inside a section.

use std::time::Duration;

use crate::golden_doc;
use crate::output::{Doc, Role};

// 1. KvBlock alignment with 1 key
golden_doc!(bucket_f, kv_one_key, |p, cap| {
    p.kv_block([("Foo", "bar")]);
});

// 2. KvBlock alignment with 2 keys
golden_doc!(bucket_f, kv_two_keys, |p, cap| {
    p.kv_block([("Foo", "1"), ("Bar", "2")]);
});

// 3. KvBlock alignment with many keys
golden_doc!(bucket_f, kv_many_keys, |p, cap| {
    p.kv_block([
        ("a", "1"),
        ("ab", "2"),
        ("abc", "3"),
        ("abcd", "4"),
        ("abcde", "5"),
        ("abcdef", "6"),
    ]);
});

// 4. Long key (>24 chars) wraps value
golden_doc!(bucket_f, long_key_wraps_value, |p, cap| {
    p.kv_block([("ThisIsAVeryLongKeyThatExceedsTwentyFourChars", "value")]);
});

// 5. Consecutive standalone kv calls auto-batch
golden_doc!(bucket_f, consecutive_kvs_align, |p, cap| {
    p.kv("Foo", "1");
    p.kv("LongerKey", "2");
    p.heading("Next"); // forces flush
});

// 6. Spinner → Status with detail + duration chain
golden_doc!(
    bucket_f,
    spinner_finish_with_detail_and_duration,
    |p, cap| {
        let sp = p.spinner("doing work");
        sp.finish_ok("done")
            .detail("note")
            .duration(Duration::from_millis(1234));
    }
);

// 7. Nested sections at 3 levels deep
golden_doc!(bucket_f, three_level_nesting, |p, cap| {
    let l1 = p.section("L1");
    let l2 = l1.section("L2");
    let l3 = l2.section("L3");
    l3.bullet("deep");
});

// 8. Empty plain section renders (none) placeholder
golden_doc!(bucket_f, empty_plain_section_shows_none, |p, cap| {
    let _s = p.section("EmptyPlain");
});

// 9. Section_if_nonempty skips when empty (in buffered Doc)
golden_doc!(bucket_f, section_if_nonempty_skips_empty, |p, cap| {
    let doc = Doc::new()
        .heading("X")
        .section_if_nonempty::<i32, _>("Items", &[], |s, _| s);
    p.emit(doc);
});

// 10. with_data divergence: human render shows heading, JSON shows payload
golden_doc!(bucket_f, with_data_divergence_human_side, |p, cap| {
    #[derive(serde::Serialize)]
    struct Pay {
        x: i32,
    }
    let doc = Doc::new()
        .heading("Visible")
        .kv("k", "v")
        .with_data(Pay { x: 7 });
    p.emit(doc);
    // Human side: heading + kv. JSON side asserted in a separate test.
});

// 11. Status with target path (dim)
golden_doc!(bucket_f, status_with_target, |p, cap| {
    p.status(Role::Warn, "outdated")
        .target(std::path::Path::new("/etc/foo.conf"));
});

// 12. Hint inside a section (indented)
golden_doc!(bucket_f, hint_inside_section, |p, cap| {
    let s = p.section("Tips");
    s.hint("run cfgd apply");
});
