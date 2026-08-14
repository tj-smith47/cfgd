---
paths: ["crates/**/*.rs"]
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
| **Colour** — a styled render used to re-read `console::colors_enabled()`, which is on under a pty | a `Printer::for_test*` constructor (all pin `colors: false`); `for_test_with_theme_colored` is the one that pins it ON |
| **Live region** — a spinner's start line is written when there is none and repainted away when there is | a `Printer::for_test*` constructor (all pin `live_region: false`); `for_test_with_live_bars` is the one that pins it ON |
| **stdin TTY** — the interactive-script gate | `execute_script_with_tty(stdin_is_tty, …)`, never the `execute_script` wrapper that reads `stdin().is_terminal()` |

Colour is decided ONCE, per `Printer`, at construction, and folded into its theme
(`Theme::with_colors`), so a capture buffer cannot be styled by construction rather
than merely stripped by convention. Production supplies the decision as a
`ColorChoice` (`Auto` resolves `console`'s detection minus
`output::printer::colors_must_be_disabled(&format)`; `--no-color` passes `Never`);
every capture constructor supplies `false`. No PRODUCTION code writes `console`'s colour
flags, so nothing a run does can change what a printer already decided. Tests write them
through exactly one guard — `output::printer::ColorGlobalOn`, which restores the prior
values on drop including on unwind — and only to reproduce the flags being ON as the
reported condition: `a_flipped_colour_global_cannot_style_a_capture` proves a capture
stays unstyled anyway, `a_colourless_printer_draws_a_colourless_progress_bar` proves
indicatif's own template resolution does not leak colour past `--no-color`, and
`derived_printers_inherit_the_colour_decision` proves a derived printer does not re-read
them. Never hand-roll a second save/restore struct; pair the guard with
`serial_test::serial`.

Strip anyway when the assertion is about TEXT: `captured_text` is still the ONE read of
a capture buffer, because `for_test_with_theme_colored` really does emit escapes and an
attribute-carrying slot emits SGR even with colour off (NO_COLOR governs colour only).
Read the buffer raw only when the assertion is ABOUT the escapes; to assert the colour
DECISION call `colors_must_be_disabled(&format)` and render nothing.

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
