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

use std::sync::{Arc, Mutex};
use std::time::Duration;

pub use theme::Theme;

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
}
