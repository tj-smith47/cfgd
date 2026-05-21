//! Snapshot tests for the output renderer.
//!
//! Goldens live in `snapshots/`. Refresh by running:
//!   INSTA_UPDATE=always cargo test -p cfgd-core --features test-helpers output::tests
//!
//! Organized into 7 buckets covering ~91 cases — baseline, verbosity,
//! status × role, themes, indent, corners, and regression.

#[cfg(feature = "test-helpers")]
mod bucket_a_baseline;
#[cfg(feature = "test-helpers")]
mod bucket_b_verbosity;
#[cfg(feature = "test-helpers")]
mod bucket_c_status_role;
#[cfg(feature = "test-helpers")]
mod bucket_d_themes;
#[cfg(feature = "test-helpers")]
mod bucket_e_indent;
#[cfg(feature = "test-helpers")]
mod bucket_f_corners;
#[cfg(feature = "test-helpers")]
mod bucket_g_regression;

/// Macro: build a Printer via `for_test_doc()`, run the body with `&p` and
/// `&cap`, then assert against `snapshots/<bucket>/<name>.txt`.
#[macro_export]
macro_rules! golden_doc {
    ($bucket:ident, $name:ident, |$p:ident, $cap:ident| $body:block) => {
        #[test]
        fn $name() {
            let ($p, $cap) = $crate::output::Printer::for_test_doc();
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
            let ($p, buf) = $crate::output::Printer::for_test_at($verbosity);
            $body
            $p.flush();
            let raw = buf.lock().unwrap().clone();
            let actual = $crate::output::strip_ansi(&raw);
            let path = std::path::Path::new("src/output/tests/snapshots")
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

/// Macro: like `golden_at!` but with an explicit Theme preset (Normal verbosity).
/// Used by bucket (d) to render a representative Doc against every preset.
/// Strips ANSI codes before snapshot comparison so color-only theme differences
/// collapse — preset divergence comes from glyph swaps (e.g., `minimal`).
#[macro_export]
macro_rules! golden_themed {
    ($bucket:ident, $name:ident, $theme_name:expr, |$p:ident| $body:block) => {
        #[test]
        fn $name() {
            let ($p, buf) = $crate::output::Printer::for_test_with_theme(
                $crate::output::Theme::from_preset($theme_name),
                $crate::output::Verbosity::Normal,
            );
            $body
            $p.flush();
            let raw = buf.lock().unwrap().clone();
            let actual = $crate::output::strip_ansi(&raw);
            let path = std::path::Path::new("src/output/tests/snapshots")
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
