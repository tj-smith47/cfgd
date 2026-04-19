//! Controlled `kubectl` shell-out helpers for the cfgd CLI plugin surface.
//!
//! Centralizing every `kubectl` invocation here (a) keeps `std::process::Command`
//! calls inside the `cli/` module-boundaries allow-list, and (b) gives us a
//! single audited site for stdio wiring, exit-code handling, and future
//! instrumentation (metrics, timing). Callers are `cli/plugin.rs`
//! (`cfgd plugin exec`, `cfgd plugin inject`) and any future kubectl-based
//! CLI commands — do NOT inline `Command::new("kubectl")` elsewhere.

use std::process::{Command, Stdio};

/// Run `kubectl` with `args`, inheriting stdio. Returns the exit code the
/// process produced. Errors only on spawn failure (kubectl not on PATH, etc).
pub fn run_inherit(args: &[&str]) -> std::io::Result<i32> {
    let status = Command::new("kubectl")
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    Ok(status.code().unwrap_or(1))
}

/// Run an arbitrary argv with inherited stdio — used for `cfgd plugin exec`
/// where argv[0] is already `kubectl` but was built as a full vector. Keeps
/// the `std::process::Command` allocation out of `plugin.rs`.
pub fn run_argv_inherit(argv: &[String]) -> std::io::Result<i32> {
    if argv.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "empty argv",
        ));
    }
    let status = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    Ok(status.code().unwrap_or(1))
}
