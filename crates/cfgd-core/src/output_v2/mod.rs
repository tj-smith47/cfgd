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

#[cfg(test)]
mod tests {
    #[test]
    fn module_compiles() {
        assert_eq!(2 + 2, 4);
    }
}
