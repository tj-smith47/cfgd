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

/// Run `kubectl` with `args`, feeding `stdin_data` on stdin and inheriting
/// stdout/stderr. Used by `kubectl cfgd deploy --apply` to `kubectl apply -f -`.
pub fn run_with_stdin(args: &[&str], stdin_data: &str) -> std::io::Result<i32> {
    run_with_stdin_at("kubectl", args, stdin_data)
}

/// Inner of [`run_with_stdin`] parameterized on the binary so the success path
/// is testable (drive it through `/usr/bin/cat`) without kubectl on PATH.
fn run_with_stdin_at(bin: &str, args: &[&str], stdin_data: &str) -> std::io::Result<i32> {
    use std::io::Write;
    let mut child = Command::new(bin)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;
    // Drop of the borrowed handle after write closes the pipe → the child sees EOF.
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(stdin_data.as_bytes())?;
    }
    let status = child.wait()?;
    Ok(status.code().unwrap_or(1))
}

/// Like [`run_with_stdin`] but CAPTURES stdout (returned) while inheriting stderr.
/// Used by `kubectl cfgd deploy --apply` in structured-output mode so kubectl's
/// human output doesn't corrupt the JSON/YAML stream.
pub fn run_with_stdin_capture_stdout(
    args: &[&str],
    stdin_data: &str,
) -> std::io::Result<(i32, String)> {
    run_with_stdin_capture_stdout_at("kubectl", args, stdin_data)
}

/// Inner of [`run_with_stdin_capture_stdout`] parameterized on the binary so the
/// success path is testable (drive it through `/usr/bin/cat`) without kubectl on PATH.
fn run_with_stdin_capture_stdout_at(
    bin: &str,
    args: &[&str],
    stdin_data: &str,
) -> std::io::Result<(i32, String)> {
    use std::io::Write;
    let mut child = Command::new(bin)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    // Drop of the borrowed handle after write closes the pipe → the child sees EOF.
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(stdin_data.as_bytes())?;
    }
    let out = child.wait_with_output()?;
    let code = out.status.code().unwrap_or(1);
    Ok((code, String::from_utf8_lossy(&out.stdout).into_owned()))
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
        // `/usr/bin/true` is silent and exits 0 on every POSIX system —
        // present on both Linux (coreutils) and macOS (BSD). `/bin/true`
        // does NOT exist on modern macOS, so prefer the /usr/bin path.
        // If it's absent we skip rather than fail — this test is about
        // exercising the success branch, not the platform.
        if !std::path::Path::new("/usr/bin/true").exists() {
            return;
        }
        let code = run_argv_inherit(&["/usr/bin/true".to_string()]).expect("spawn");
        assert_eq!(code, 0);
    }

    #[test]
    fn run_argv_inherit_with_false_binary_returns_exit_one() {
        if !std::path::Path::new("/usr/bin/false").exists() {
            return;
        }
        let code = run_argv_inherit(&["/usr/bin/false".to_string()]).expect("spawn");
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
        // Excludes concurrent script-interpreter spawns (which resolve `sh` via
        // PATH) for the whole empty-PATH window; held until end of scope.
        let _spawn_excl = cfgd_core::test_helpers::path_env_mutation_guard();
        // SAFETY: spawn exclusion above + serial gate ⇒ no concurrent PATH reader.
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

    #[test]
    fn run_with_stdin_at_cat_consumes_stdin_and_exits_zero() {
        // `cat` reads stdin until EOF then exits 0 — proves the pipe is wired
        // and closed correctly so the child does not hang waiting on more input.
        if !std::path::Path::new("/usr/bin/cat").exists() {
            return;
        }
        let code = run_with_stdin_at("/usr/bin/cat", &[], "hello from cfgd\n")
            .expect("spawn cat with piped stdin");
        assert_eq!(code, 0, "cat must exit 0 after consuming stdin");
    }

    #[test]
    fn run_with_stdin_at_nonexistent_binary_returns_not_found() {
        let err = run_with_stdin_at("/no/such/binary-cfgd-test", &[], "data")
            .expect_err("spawn of missing binary must Err");
        assert!(
            matches!(err.kind(), std::io::ErrorKind::NotFound),
            "expected NotFound, got {err:?}"
        );
    }

    #[test]
    fn run_with_stdin_capture_stdout_at_cat_round_trips_stdin() {
        // `cat` echoes its stdin to stdout — the capture variant must return
        // exit 0 AND the exact bytes back, proving stdout is piped (not inherited).
        if !std::path::Path::new("/usr/bin/cat").exists() {
            return;
        }
        let (code, out) =
            run_with_stdin_capture_stdout_at("/usr/bin/cat", &[], "hello from cfgd\n")
                .expect("spawn cat with piped stdin+stdout");
        assert_eq!(code, 0, "cat must exit 0");
        assert_eq!(
            out, "hello from cfgd\n",
            "captured stdout must round-trip stdin"
        );
    }

    #[test]
    fn run_with_stdin_capture_stdout_at_nonexistent_binary_returns_not_found() {
        let err = run_with_stdin_capture_stdout_at("/no/such/binary-cfgd-test", &[], "data")
            .expect_err("spawn of missing binary must Err");
        assert!(
            matches!(err.kind(), std::io::ErrorKind::NotFound),
            "expected NotFound, got {err:?}"
        );
    }
}
