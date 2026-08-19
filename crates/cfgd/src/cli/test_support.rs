//! Shared test-only assertions for `cli/` unit tests. Not built into any
//! shipped binary — every item here is `#[cfg(test)]`.

/// Assert that `needle`'s line sits exactly one section level (2 spaces —
/// see `Renderer::indent_prefix`) deeper than `header`'s own line: the shape
/// every QP9 depth fix must hold, a settled action line nesting DIRECTLY
/// under the section/owner header that introduced it, not merely somewhere
/// deeper than it. `output` is ANSI-stripped human text.
///
/// Six call sites shared this block by copy before QP9 round 1 consolidated
/// them — three in-crate (`cli/checkin.rs` twice, `cli/module/build.rs`,
/// `cli/module/push_pull.rs`) and two integration-test copies (their sibling
/// in `tests/common::assert_nests_under` covers those, since an integration
/// test cannot reach a `pub(crate)` item in the binary crate). Keep both in
/// sync if the nesting contract ever changes.
#[cfg(test)]
pub(crate) fn assert_nests_under(output: &str, header: &str, needle: &str) {
    let header_line = output
        .lines()
        .find(|l| l.trim_start() == header)
        .unwrap_or_else(|| panic!("{header:?} header must be rendered: {output}"));
    let settled_line = output
        .lines()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} settle line must be rendered: {output}"));

    let header_indent = header_line.len() - header_line.trim_start().len();
    let settled_indent = settled_line.len() - settled_line.trim_start().len();
    assert_eq!(
        settled_indent,
        header_indent + 2,
        "the settle line must nest exactly one section level (2 spaces) \
         under its header, not merely somewhere deeper \
         (header indent {header_indent}, settle indent {settled_indent}): {output}"
    );
}

#[cfg(test)]
mod tests {
    use super::assert_nests_under;

    /// `assert_nests_under` was consolidated from six call sites that each
    /// asserted only `settled_indent > header_indent` — a check a REGRESSION
    /// to two levels deep (or zero, sitting flush) would have slipped past.
    /// Proves the consolidated helper actually rejects both wrong depths, not
    /// just the correct one.
    #[test]
    #[should_panic(expected = "must nest exactly one section level")]
    fn rejects_a_settle_line_nested_two_levels_instead_of_one() {
        let output = "Push\n    \u{2713} Pushed module to registry\n";
        assert_nests_under(output, "Push", "Pushed module to");
    }

    #[test]
    #[should_panic(expected = "must nest exactly one section level")]
    fn rejects_a_settle_line_sitting_flush_with_its_header() {
        let output = "Push\n\u{2713} Pushed module to registry\n";
        assert_nests_under(output, "Push", "Pushed module to");
    }

    #[test]
    fn accepts_a_settle_line_nested_exactly_one_level() {
        let output = "Push\n  \u{2713} Pushed module to registry\n";
        assert_nests_under(output, "Push", "Pushed module to");
    }
}
