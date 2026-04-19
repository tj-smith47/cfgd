//! Bump the Windows PE stack reservation for the `cfgd` binary at link time.
//!
//! Windows PE binaries default to a 1 MiB main-thread stack. cfgd's clap
//! `Command` tree — 29 top-level subcommands, each carrying `long_about` with
//! `Examples:` blocks, some built via `format!` — allocates enough transient
//! state during `Cli::parse_from` to overflow that. Windows CI confirmed it:
//! every `cfgd.exe` invocation exited with `STATUS_STACK_OVERFLOW`
//! (`-1073741571` = `0xC00000FD`) and "main has overflowed its stack".
//!
//! Linux / macOS default to 8 MiB, so this is Windows-specific. Setting the PE
//! header's reserved-stack field at link time has zero runtime cost (no
//! thread::spawn, no join, no extra allocation) — the Windows loader just
//! allocates a bigger initial stack for the main thread.
//!
//! MSVC toolchain takes `/STACK:<bytes>`; the GNU/MinGW linker takes
//! `-Wl,--stack,<bytes>`. Emit both forms and let cargo route to whichever
//! applies based on the target-env config.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        return;
    }

    // 8 MiB — matches the Linux default so behavior is consistent across
    // platforms and comfortably absorbs any further clap-surface growth.
    const STACK_BYTES: usize = 8 * 1024 * 1024;

    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    match target_env.as_str() {
        "msvc" => {
            println!("cargo:rustc-link-arg-bin=cfgd=/STACK:{STACK_BYTES}");
        }
        "gnu" => {
            println!("cargo:rustc-link-arg-bin=cfgd=-Wl,--stack,{STACK_BYTES}");
        }
        _ => {
            // Unknown Windows target env — skip silently; the linker default
            // will apply. Worst case the clap overflow recurs and CI catches
            // it, but we don't want to pass an incompatible flag here.
        }
    }
}
