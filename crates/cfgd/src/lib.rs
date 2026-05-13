//! cfgd library surface — exposes the CLI entry point so external integration
//! tests can build the clap tree without spawning the binary. The binary at
//! `src/main.rs` is a thin wrapper over this library.

pub mod ai;
pub mod cli;
pub mod files;
pub mod generate;
pub mod mcp;
pub mod packages;
pub mod secrets;
pub mod system;

pub use cli::Cli;
