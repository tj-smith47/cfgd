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

#[cfg(test)]
#[cfg(unix)]
mod tests {
    //! Unit coverage for the kubectl shell-out helpers. We drive `run_argv_inherit`
    //! through `/bin/true`, `/bin/false`, and a nonexistent binary so the
    //! Ok(0) / Ok(non-zero) / spawn-Err arms each fire. The `run_inherit`
    //! entry deliberately scrubs PATH to pin its spawn-Err arm without
    //! shelling out to a real kubectl.
    use super::*;
    use serial_test::serial;

    #[test]
    fn run_argv_inherit_with_empty_argv_returns_invalid_input_err() {
        let err = run_argv_inherit(&[]).expect_err("empty argv must Err");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn run_argv_inherit_with_true_binary_returns_exit_zero() {
        // `/bin/true` is silent and exits 0 on every POSIX system. If it's
        // absent we skip rather than fail — this test is about exercising
        // the success branch, not the platform.
        if !std::path::Path::new("/bin/true").exists() {
            return;
        }
        let code = run_argv_inherit(&["/bin/true".to_string()]).expect("spawn");
        assert_eq!(code, 0);
    }

    #[test]
    fn run_argv_inherit_with_false_binary_returns_exit_one() {
        if !std::path::Path::new("/bin/false").exists() {
            return;
        }
        let code = run_argv_inherit(&["/bin/false".to_string()]).expect("spawn");
        assert_eq!(code, 1);
    }

    #[test]
    fn run_argv_inherit_with_nonexistent_binary_returns_spawn_err() {
        let err = run_argv_inherit(&["/no/such/binary-cfgd-test".to_string()])
            .expect_err("spawn of missing binary must Err");
        // ENOENT on Unix; ErrorKind::NotFound is the cross-platform mapping.
        assert!(
            matches!(err.kind(), std::io::ErrorKind::NotFound),
            "expected NotFound, got {err:?}"
        );
    }

    #[test]
    #[serial]
    fn run_inherit_returns_spawn_err_when_kubectl_not_on_path() {
        // Force kubectl not findable so we hit the spawn-Err branch
        // deterministically. We can't assert on its successful-spawn arm
        // without polluting test stdout, so we pin only the Err path here.
        let prior_path = std::env::var_os("PATH");
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: serial_test::serial gates execution; no concurrent readers.
        unsafe {
            std::env::set_var("PATH", tmp.path());
        }
        let result = run_inherit(&["version"]);
        unsafe {
            match prior_path {
                Some(p) => std::env::set_var("PATH", p),
                None => std::env::remove_var("PATH"),
            }
        }
        let err = result.expect_err("kubectl missing from PATH → Err");
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }
}
