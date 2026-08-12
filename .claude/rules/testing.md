---
paths: ["**/*.rs"]
---
# cfgd Testing

- `cargo test` must pass before any phase is considered complete.
- Unit tests for pure logic (config parsing, diffing, template rendering). Co-located in `#[cfg(test)] mod tests {}` within each module.
- Integration tests in `tests/`, using `assert_cmd` for CLI commands.
- Package manager tests use mock trait implementations, not real system calls.
- Use `tempfile` for any test that touches the filesystem.

## The reconciler cannot resolve a real `$HOME` under test

`Reconciler::new` resolves the home directory once, in `resolved_home()`. The
real-home arm is gated on `cfg(not(any(test, feature = "test-helpers")))` — the
feature matters, because `cfg(test)` is set only while compiling cfgd-core's
own test binary. Without it a `cfgd` / `cfgd-operator` / `cfgd-csi` test links a
release-shaped core, and a non-dry-run apply of a profile carrying `spec.env`
rewrites the operator's own `~/.cfgd.env`, `~/.config/environment.d/cfgd.conf`
and shell rc files. Each consumer enables `test-helpers` in
`[dev-dependencies]` only, so no shipped binary compiles the test arm.

A test that installs `with_test_home_guard` gets the home it asked for. A test
that installs nothing gets a throwaway directory unique to its own thread,
named `cfgd-unguarded-test-home-<pid>-<n>` under the system temp dir. That
directory is named, not created: anything appearing there is a test that writes
env surfaces and should install a guard.

Blocking dispatch loses the thread-local, so any closure that may resolve `~`
goes through `cfgd_core::spawn_blocking_with_test_home`. `audit.sh` rejects a
raw `tokio::task::spawn_blocking` anywhere in workspace production code unless
the call line (or the line above it) carries
`// spawn-blocking-ok: <why the closure resolves no home paths>`.

## A test never inherits its terminal shape from the ambient one

`cargo test` from a pipe and `script -qec "cargo test" /dev/null` (a real pty)
are two different terminals, and a test that reads either one asserts about how
the suite was started rather than about what the code did. Three ambient inputs
have to be supplied, never inherited:

| Ambient input | Supply it with |
|---|---|
| **Colour** — `console::colors_enabled()` is on under a pty | `crate::output::strip_ansi(...)` on the capture before ANY assertion |
| **Live region** — a spinner's start line is written when there is none and repainted away when there is | a `Printer::for_test*` constructor (all pin `live_region: false`); `for_test_with_live_bars` is the one that pins it ON |
| **stdin TTY** — the interactive-script gate | `execute_script_with_tty(stdin_is_tty, …)`, never the `execute_script` wrapper that reads `stdin().is_terminal()` |

Colour is the trap, because it is process-global AND mutable: `ColorsEnabledGuard::set(true)`
in a themes test flips it for every non-serial test running beside it, so even a
pipe-invoked suite can style a capture. Strip, always — a `contains("✓ Foo")` breaks
when an escape lands between the icon and the subject, and the negative form
`!contains("✓ Foo")` passes vacuously, silently stopping guarding anything. Reach for
`ColorsEnabledGuard` only when the assertion is ABOUT the escapes; to assert the colour
DECISION use `output::printer::colors_must_be_disabled(&format)` and take no guard.

Goldens are captured through a path where all three are pinned — `assert_human_snapshot*`
strips for its caller, while the raw `assert_snapshot_at` does not, so a caller reaching
it directly strips first. Goldens are RE-CAPTURED (`INSTA_UPDATE=always`), never
hand-edited.

Verify both ways before calling a test suite green; a suite only ever observed one way is
how all of this shipped.

## Fixture versions: use the 9.9.x sentinel range

When a test hardcodes a version string as a scaffold (mock upgrade
flows, fake release tags, illustrative bump scenarios) rather than
asserting against `CARGO_PKG_VERSION`, use a version in the **9.9.x**
range (e.g. `v9.9.0`, `v9.9.1`). These never coincide with any real
cfgd release stream, so the test stays inert across version bumps.

A real bump (`0.3.5 → 0.4.0`) once silently broke `upgrade_bridge_one_blank_line`
because the test body hardcoded `"Upgraded to v0.4.0"` as a fixture
that happened to match the project's actual target. Reverting the
project version flipped the test red even though nothing about the
*formatting invariant* the test claimed to check had changed.

Tests that DO assert against real `CARGO_PKG_VERSION` (e.g.
`upgrade_check_up_to_date_human` exercising `cmd_upgrade`) keep their
snapshots tracking the real version — those are correctly coupled.
The sentinel rule applies only to test-body literal fixtures.
