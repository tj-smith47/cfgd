//! A [`crate::output::KvPair::nested`] row breaks down the row above it, and
//! reaches the same rendered bytes down BOTH surfaces — the buffered `Doc`
//! tree and the streaming `SectionGuard` — because a compact report built one
//! way and a section built the other must not indent the same breakdown
//! differently.
use crate::output::{Doc, KvPair, Printer, Verbosity};

fn rows() -> Vec<KvPair> {
    vec![
        KvPair::new("Files", "12"),
        KvPair::new("Scripts", "7"),
        KvPair::nested("preApply", "1"),
        KvPair::nested("postApply", "6"),
    ]
}

/// The value column of a nested row lines up with the values of the rows it
/// sits under: the key column is measured over the INDENTED width, so the
/// indent comes out of the key's own padding rather than pushing the value two
/// columns right.
fn assert_aligned(out: &str) {
    // No key in the fixture carries a digit, so the first one is the value.
    let value_col = |line: &str| line.find(|c: char| c.is_ascii_digit());
    let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 4, "expected four rows, got: {out:?}");
    assert!(
        lines[2].starts_with("  preApply") && lines[3].starts_with("  postApply"),
        "a nested row is indented two columns in: {out:?}"
    );
    assert!(
        !lines[0].starts_with(' ') && !lines[1].starts_with(' '),
        "an ordinary row keeps the block's own key column: {out:?}"
    );
    let cols: Vec<Option<usize>> = lines.iter().map(|l| value_col(l)).collect();
    assert!(
        cols.iter().all(|c| *c == cols[0]),
        "values must share one column, got {cols:?} in {out:?}"
    );
}

#[test]
fn a_doc_kv_block_indents_its_nested_rows_and_keeps_one_value_column() {
    let (p, buf) = Printer::for_test_at(Verbosity::Normal);
    p.emit(Doc::new().kv_rows(rows()));
    p.flush();
    assert_aligned(&crate::test_helpers::captured_text(&buf));
}

#[test]
fn a_streaming_kv_block_indents_its_nested_rows_the_same_way() {
    let (p, buf) = Printer::for_test_at(Verbosity::Normal);
    {
        let section = p.section("Declared");
        section.kv_rows(rows());
    }
    p.flush();
    let out = crate::test_helpers::captured_text(&buf);
    // The section heading and its indent are the streaming path's own; the
    // breakdown below it is what must match.
    let block: String = out
        .lines()
        .skip_while(|l| !l.contains("Files"))
        .map(|l| format!("{}\n", l.strip_prefix("  ").unwrap_or(l)))
        .collect();
    assert_aligned(&block);
}

/// Nesting is not free: a nested row renders differently from the flat row it
/// would otherwise have been, so the indent is really reaching the renderer.
///
/// The other half of the claim — that an UNNESTED block still renders the bytes
/// it always did — is carried by the `status` and `module` goldens, which were
/// not re-captured when this slot landed. It cannot be asserted here: rendering
/// the same input twice and comparing the two proves only that the renderer is
/// deterministic.
#[test]
fn a_nested_row_renders_differently_from_the_flat_row_beside_it() {
    let render = |pairs: Vec<KvPair>| {
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        p.emit(Doc::new().kv_rows(pairs));
        p.flush();
        crate::test_helpers::captured_text(&buf)
    };
    assert_ne!(
        render(vec![KvPair::new("Files", "12"), KvPair::nested("a", "1")]),
        render(vec![KvPair::new("Files", "12"), KvPair::new("a", "1")]),
        "a nested row must not render identically to a flat one"
    );
}
