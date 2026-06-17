//! Cross-platform fake `cosign` binary for cfgd's hermetic signing tests.
//!
//! Production cosign shell-out flows through [`cfgd_core::cosign_cmd`], which
//! honors the `CFGD_COSIGN_BIN` override seam (see
//! `.claude/rules/module-boundaries.md`). Tests point that seam at *this*
//! compiled binary instead of a real cosign, so the sign / attest / verify
//! code paths run end-to-end without ever touching the network or a real
//! Sigstore key — identically on Linux, macOS, and Windows.
//!
//! A single artifact serves every test variant: its per-invocation behavior is
//! supplied through env vars set by `CosignTestShim::install()` rather than
//! baked into the binary. This mirrors the prior Unix-only `/bin/sh` shim
//! byte-for-byte:
//!
//! | Env var                   | Effect                                                  |
//! |---------------------------|---------------------------------------------------------|
//! | `CFGD_FAKE_COSIGN_LOG`    | When set, append the space-joined argv + `\n` to the path (argv logging). |
//! | `CFGD_FAKE_COSIGN_KEYGEN`| When `1`, and argv[0] is `generate-key-pair`, write fake `cosign.key`/`cosign.pub` to CWD. |
//! | `CFGD_FAKE_COSIGN_STDERR`| Written verbatim to this process's stderr on every invocation. |
//! | `CFGD_FAKE_COSIGN_EXIT`  | Parsed as the process exit code (default `0`).          |
//!
//! The binary never inspects which cosign subcommand it was handed beyond the
//! `generate-key-pair` keygen branch — exactly like the script it replaces.

use std::io::Write as _;

const LOG_ENV: &str = "CFGD_FAKE_COSIGN_LOG";
const KEYGEN_ENV: &str = "CFGD_FAKE_COSIGN_KEYGEN";
const STDERR_ENV: &str = "CFGD_FAKE_COSIGN_STDERR";
const EXIT_ENV: &str = "CFGD_FAKE_COSIGN_EXIT";

fn main() {
    // Drop argv[0] (the binary path); keep only the cosign-style arguments,
    // matching `"$*"` / `"$1"` in the shell shim.
    let args: Vec<String> = std::env::args().skip(1).collect();

    // Argv logging: one space-joined line per invocation, appended in order.
    if let Ok(log_path) = std::env::var(LOG_ENV)
        && let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
    {
        let _ = writeln!(f, "{}", args.join(" "));
    }

    // Keygen: emulate `cosign generate-key-pair` writing its key files to CWD.
    let keygen = std::env::var(KEYGEN_ENV).as_deref() == Ok("1");
    if keygen && args.first().map(String::as_str) == Some("generate-key-pair") {
        let _ = std::fs::write("cosign.key", b"fake-private-key-bytes");
        let _ = std::fs::write("cosign.pub", b"fake-public-key-bytes");
    }

    // Configured stderr on every invocation (empty by default).
    if let Ok(stderr) = std::env::var(STDERR_ENV) {
        let _ = std::io::stderr().write_all(stderr.as_bytes());
    }

    let code = std::env::var(EXIT_ENV)
        .ok()
        .and_then(|v| v.parse::<i32>().ok())
        .unwrap_or(0);
    std::process::exit(code);
}
