//! New typed-component output system. Ships alongside `output/` during the R1–R3
//! migration; replaces it at R3.
//!
//! See `.claude/specs/2026-05-14-output-system-redesign-design.md` for the design.

pub mod role;
pub use role::Role;

pub mod verbosity;
pub use verbosity::{OutputFormat, Verbosity};

pub mod theme;
pub use theme::Theme;

pub mod component;
pub use component::{Component, KvPair};

pub mod renderer;

pub mod printer;
pub use printer::{DocCapture, Printer, PromptAnswer};

pub mod section_guard;
pub use section_guard::SectionGuard;

pub mod status_builder;
pub use status_builder::StatusBuilder;

pub mod spinner;
pub use spinner::{ProgressBar, Spinner};

pub mod process;
pub use process::CommandOutput;

pub mod prompts;

pub mod raw;

pub mod doc;
pub use doc::{Doc, SectionBuilder, StatusFields};

/// Build a stable-shaped error Doc for `bail!`-on-emit-then-fail sites.
/// Carries an `error` category key + `name` so structured consumers
/// (`-o json`) see a consistent payload on failure. Any extra fields in
/// `extras` (object literal expected) are merged into the payload alongside
/// `error` + `name`.
pub fn error_doc(
    name: &str,
    error_kind: &str,
    message: impl Into<String>,
    extras: serde_json::Value,
) -> Doc {
    let mut payload = serde_json::json!({
        "error": error_kind,
        "name": name,
    });
    if let serde_json::Value::Object(extra_map) = extras
        && let serde_json::Value::Object(payload_map) = &mut payload
    {
        for (k, v) in extra_map {
            payload_map.insert(k, v);
        }
    }
    Doc::new().status(Role::Fail, message).with_data(payload)
}

pub mod render_doc;

pub mod structured;

#[cfg(feature = "test-helpers")]
pub mod test_capture;

#[cfg(test)]
mod tests;
