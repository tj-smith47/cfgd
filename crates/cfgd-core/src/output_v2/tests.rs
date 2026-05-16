//! Snapshot tests for the output_v2 renderer.
//!
//! Goldens live in `snapshots/`. Refresh by running:
//!   INSTA_UPDATE=always cargo test -p cfgd-core --features test-helpers output_v2::tests
//!
//! See spec §15.1 for the prioritization (7 buckets, ~91 cases).

#[cfg(feature = "test-helpers")]
mod bucket_a_baseline;
#[cfg(feature = "test-helpers")]
mod bucket_b_verbosity;
// later: bucket_c_status_role, bucket_d_themes,
// bucket_e_indent, bucket_f_corners, bucket_g_regression

/// Macro: build a Printer via `for_test_doc()`, run the body with `&p` and
/// `&cap`, then assert against `snapshots/<bucket>/<name>.txt`.
#[macro_export]
macro_rules! golden_doc {
    ($bucket:ident, $name:ident, |$p:ident, $cap:ident| $body:block) => {
        #[test]
        fn $name() {
            let ($p, $cap) = $crate::output_v2::Printer::for_test_doc();
            $body
            $p.flush();
            $cap.assert_human_snapshot(&format!(
                "{}/{}.txt",
                stringify!($bucket),
                stringify!($name)
            ));
        }
    };
}

/// Macro: like `golden_doc!` but with explicit Verbosity. Used by bucket (b)
/// to test verbosity gating without `for_test_doc()`'s default Normal.
/// Strips ANSI codes before snapshot comparison.
#[macro_export]
macro_rules! golden_at {
    ($bucket:ident, $name:ident, $verbosity:expr, |$p:ident| $body:block) => {
        #[test]
        fn $name() {
            let ($p, buf) = $crate::output_v2::Printer::for_test_at($verbosity);
            $body
            $p.flush();
            let raw = buf.lock().unwrap().clone();
            let actual = $crate::output_v2::tests::strip_ansi(&raw);
            let path = std::path::Path::new("src/output_v2/tests/snapshots")
                .join(stringify!($bucket))
                .join(format!("{}.txt", stringify!($name)));
            if std::env::var("INSTA_UPDATE").as_deref() == Ok("always") || !path.exists() {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, &actual).unwrap();
                return;
            }
            let expected = std::fs::read_to_string(&path).unwrap();
            pretty_assertions::assert_eq!(
                actual, expected, "snapshot mismatch: {}", stringify!($name));
        }
    };
}

/// ANSI-stripping helper used by `golden_at!` and by every per-module
/// `#[cfg(test)] mod tests` block under `output_v2/`. Lives at module scope so
/// all buckets and sibling tests can reach it via
/// `crate::output_v2::tests::strip_ansi`.
///
/// Chars-based so it is safe across multi-byte UTF-8 glyphs (`✓ ✗ — →`). ANSI
/// CSI sequences are all ASCII, so we can walk chars and skip them without
/// splitting glyphs.
pub(crate) fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next(); // consume '['
            for inner in chars.by_ref() {
                if inner == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}
