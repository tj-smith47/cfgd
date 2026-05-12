// Centralized output system — all terminal interaction goes through Printer

mod diff;
mod printer;
mod process;
mod progress;
mod structured;
mod syntax;
mod theme;

#[cfg(test)]
mod tests;

use console::Term;
use indicatif::MultiProgress;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub use theme::Theme;

/// A canned response for one prompt invocation. Used by tests to drive
/// command flows past `prompt_confirm` / `prompt_text` / `prompt_select`
/// without an attached TTY. Each prompt call consumes one queue entry;
/// type-mismatched or exhausted entries fall back to the normal
/// non-interactive Err arm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptAnswer {
    Confirm(bool),
    Text(String),
    Select(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputFormat {
    Table,
    Wide,
    Json,
    Yaml,
    Name,
    Jsonpath(String),
    Template(String),
    TemplateFile(std::path::PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verbosity {
    Quiet,
    Normal,
    Verbose,
}

/// Result of running a command with live output display.
pub struct CommandOutput {
    pub status: std::process::ExitStatus,
    pub stdout: String,
    pub stderr: String,
    pub duration: Duration,
}

pub struct Printer {
    theme: Theme,
    term: Term,
    multi_progress: MultiProgress,
    syntax_set: SyntaxSet,
    theme_set: ThemeSet,
    verbosity: Verbosity,
    output_format: OutputFormat,
    /// Optional buffer for capturing output in tests. When set, all output
    /// methods append plain text here regardless of verbosity level.
    test_buf: Option<Arc<Mutex<String>>>,
    /// Optional queue of canned prompt responses for tests. When set, each
    /// `prompt_confirm` / `prompt_text` / `prompt_select` call pops the
    /// front entry and returns it (if the type matches). Production
    /// Printers leave this None and prompts behave normally.
    prompt_queue: Option<Arc<Mutex<VecDeque<PromptAnswer>>>>,
}
