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

That fallback is the RECONCILER's alone. The env CHECK
(`env_verify_results`, and the `~/`-fold every report path passes through)
resolves `~` with `expand_tilde`, which has no unguarded-test arm and reads the
real `$HOME`: an in-process test that declares `spec.env`/`spec.aliases` and
runs a `cmd_*` reports the invoking user's own env surface. That passes on a
development box, which dogfoods cfgd and already holds every planned target,
and fails on a CI runner whose `$HOME` holds none — so such a test installs
`with_test_home_guard` and plants its surface through
`MergedEnvItems::managed_env_files` / `managed_env_source_lines`.
`every_in_process_test_declaring_shell_items_holds_a_test_home` walks every
crate's `tests/`.

Blocking dispatch loses the thread-local, so any closure that may resolve `~`
goes through `cfgd_core::spawn_blocking_with_test_home`. `audit.sh` rejects a
raw `tokio::task::spawn_blocking` anywhere in workspace production code unless
the call line (or the line above it) carries
`// spawn-blocking-ok: <why the closure resolves no home paths>`.

## No test run reaches the real config directory

The shell suites under `tests/e2e/*/scripts/` run the real binary as the
invoking user, so every path cfgd resolves from `$HOME` or `$XDG_*` is that
user's own. `tests/e2e/common/scratch-home.sh` is the ONE redirect: it exports
`HOME`, `USERPROFILE` and all four `XDG_*` directories into the run's scratch
root, asserts the redirect took before anything else reads `$HOME`, and passes
through only the seams a suite genuinely needs out of the real home
(`CARGO_HOME`, `RUSTUP_HOME`, `KUBECONFIG`, `DOCKER_CONFIG`, the three
`HELM_*`). It also fingerprints the real config directory at source time;
`assert_real_config_dir_unchanged` re-reads it and every `run-all.sh` fails the
whole run when it moved. `helpers.sh` sources it, so every suite reaches it, and
`every_e2e_suite_runs_under_the_one_scratch_home` (`crates/cfgd/src/cli/tests.rs`)
walks every `tests/e2e/*/scripts/run-all.sh` for both halves, so a new suite
directory trips over the rule.

The binary answers for its own half: a verb that materialises a config from
`--from` with no destination named refuses to write into a default config
directory that already holds a `cfgd.yaml`, is not empty, or is a symlink
(`crates/cfgd/tests/from_default_dir_refusal.rs`). Both halves exist because
neither one was enough: an e2e `apply --from` pointed at a scratch `--config`
cloned its fixture into a developer's `~/.config/cfgd`, and `secret` wrote an
age key beside it.

## A test never inherits its terminal shape from the ambient one

`cargo test` from a pipe and `script -qec "cargo test" /dev/null` (a real pty)
are two different terminals, and a test that reads either one asserts about how
the suite was started rather than about what the code did. Three ambient inputs
have to be supplied, never inherited:

| Ambient input | Supply it with |
|---|---|
| **Colour** — a styled render used to re-read `console::colors_enabled()`, which is on under a pty | a `Printer::for_test*` constructor (all pin `colors: false`); `for_test_with_theme_colored` is the one that pins it ON |
| **Live region** — a spinner's start line is written when there is none and repainted away when there is | a `Printer::for_test*` constructor (they pin `live_region: false`); the three `live_capture` constructors — `for_test_live_scrollback`, `for_test_with_live_bars`, `for_test_live_terminal` — pin it ON, and which one to reach for is in `shared-utils.md`'s Test guards section |
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
indicatif's own template resolution leaks NO escape past `--color never` — a style
token is one whatever it names, so the colourless template carries none and the
filled/empty contrast comes from `progress_chars` instead — and
`derived_printers_inherit_the_colour_decision` proves a derived printer does not re-read
them. Never hand-roll a second save/restore struct; pair the guard with
`serial_test::serial`.

The same pairing is the rule for the environment itself: a test that mutates a
process-global env var — through `EnvVarGuard`, `EditorGuard::set`,
`with_test_env_var`, `ProbePath::containing`, any `install_named_path_shim*`,
either tool shim, or a raw `env::set_var` / `remove_var`, in its own body or in a
same-file helper it calls — carries `#[serial_test::serial…]`, because edition 2024 makes
an unserialized write in a live-threaded harness undefined behaviour rather than
a flake; `every_test_mutating_the_process_environment_serializes_itself` walks
for one, and `// serial-ok: <why>` hatches a mutation that cannot race (a
per-child `Command::env(…)` handoff is not one of them and is never matched).

Two serial attributes on one declaration are two locks, taken in the order they
are written, so every declaration writes that order the same way: the unnamed
lock first, named groups alphabetically. The other order holds one lock while it
waits for the other against a sibling doing the reverse, and both tests hang with
no timeout to fire;
`every_declaration_taking_two_serial_locks_takes_them_in_one_order`
(`output/tests/fences.rs`) walks every crate for an inverted pair and has no
hatch.

Colour off means NO escapes — attributes included. `ThemedStyle::apply_to` is the ONE
gate a styled span becomes bytes through, and a printer whose `ColorChoice` resolved
`false` gets bare text: bold, dim, italic, underline and OSC 8 are withheld with the
foreground, because an attribute is styling too and `docs/cli-reference.md` promises
`--color never` / `NO_COLOR` / a non-terminal stdout withhold every escape. An indicatif
template is a second escape writer and answers to the same decision. Pinned by
`no_escape_reaches_a_stream_the_printer_decided_against`, the bar pin above, and the walk
beside them, `every_styled_span_reaches_bytes_through_the_one_gate` — whose `WRITES` list
is the CLASS (every literal notation, the raw byte, `ProgressStyle::with_template`,
`console::Style`), whose `GATED` needle is `apply_to` itself, and whose escape hatch is
`// style-gate-ok: <why>` read off the whole comment block above the line.

Strip anyway when the assertion is about TEXT: `captured_text` is still the ONE read of
a capture buffer, because `for_test_with_theme_colored` deliberately forces styling ON
and really does emit escapes. Read the buffer raw only when the assertion is ABOUT the
escapes; to assert the colour DECISION call `colors_must_be_disabled(&format)` and
render nothing.

Goldens are captured through a path where all three are pinned — `assert_human_snapshot*`
strips for its caller, while the raw `assert_snapshot_at` does not, so a caller reaching
it directly strips first. Goldens are RE-CAPTURED (`INSTA_UPDATE=always`), never
hand-edited.

Verify both ways before calling a test suite green; a suite only ever observed one way is
how all of this shipped.

## A scoped tracing capture installs the process-global journal under it

`tracing` caches one `Interest` per callsite for the whole process and computes it
from what the thread that first reaches the callsite can see. While a single
dispatcher is registered, a callsite first reached from a thread holding no
subscriber at all caches `never`, and every later event there is dropped until an
unrelated registration rebuilds the cache — including the event a capture on
another thread is waiting for, which reads back empty. So a test binding a scoped
subscriber (`tracing::subscriber::with_default`, `WithSubscriber`) calls
`cfgd_core::test_helpers::install_tracing_journal()` first, and
`every_scoped_tracing_capture_installs_the_journal_under_it`
(`output/tests/fences.rs`) walks every crate for a bind with no installer above
it.

The journal that installer leaves behind is also what the daemon's `run_daemon`
loop tests read: those lines come from tokio tasks and watcher threads the daemon
owns, which a scoped dispatcher never reaches. A reader of it asks only whether a
line APPEARED — it carries no target filter, so every event any test in the binary
emits reaches it, and an absence or a count answers by whatever else the run
scheduled. A line read as proof that the reader's OWN subject reached a state
needs more than containment: every declaration that starts a daemon joins the
`tracing_dispatcher` group, because a sibling's daemon writes the same startup
banner.

That group belongs to the journal alone. A scoped capture holds one thread's
buffer for the length of one closure, so a test whose only tracing reach is a
capture carries no serial attribute: what keeps its verdict independent of what
another thread registered is the journal installed under it, not a lock.

## A fail-without-fix probe never mutates the shared working tree

Proving a test fails without its fix means breaking the production code and watching
the test go red. Doing that **in place** — edit, `cargo test`, restore — leaves the
repository holding deliberately broken code for the length of a compile, and anything
else reading the tree in that window (a second agent, a watch build, a full-workspace
run someone else started) compiles the broken revision and reports failures that
describe nothing anybody wrote.

That is not hypothetical: two daemon advisory-restatement tests were reported failing
under a full `--test-threads=16` workspace run, with counts (`0 of 3` and `1 of 3`)
that exactly reproduced an in-tree probe of `CachedConfig::advisories_to_restate`
returning `&[]`. Six later runs of the same binary at the same thread count were green,
and the failure was never a concurrency defect at all — it was a probe window.

Copy the tree first — **excluding `target/`**, which is tens of GB and duplicating it
filled the shared VM's root filesystem to 100% mid-CI — and give the copy its own
target dir:

```bash
rsync -a --exclude=/target /opt/repos/cfgd/ ~/.cache/cfgd-debug/probe/
CARGO_TARGET_DIR=~/.cache/cfgd-debug/probe-target \
  cargo test --manifest-path ~/.cache/cfgd-debug/probe/Cargo.toml -p cfgd-core --features test-helpers --lib <filter>
```

The evidence is identical and no other reader can see the mutation. Scratch goes under
`~/.cache/`, never `/tmp`, and the probe TREE is deleted as soon as the probe's red run is
captured, so nothing later reads a tree still carrying a deliberate defect. The shared
target dir (`~/.cache/cfgd-debug/red-target`) is retained: a fresh tree is copied per
probe, so what a kept target dir changes is rebuild cost, never what a probe measures.

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

## A pin that runs at one uid says so in its name

A test whose first statement returns early on `cfgd_core::is_root()` executes nothing
at the other uid, and its green line is indistinguishable from a pin that ran. The name
carries which half executed, with a mechanical suffix read off the gate:

| First statement | Suffix |
|---|---|
| `if !is_root() { return; }` | `_as_root` |
| `if is_root() { return; }` | `_as_non_root` |
| `if !(cfg!(target_os = "linux") && is_root()) { return; }` | `_as_linux_root` |
| `if cfg!(target_os = "linux") && is_root() { return; }` | `_as_non_linux_root` |

A pin that asserts in both arms (`if is_root() { assert A } else { assert B }`) takes no
suffix: every run executes it. The suffix also binds the other way, so a name claiming a
uid must open on that gate. `every_pin_that_runs_at_one_uid_says_so_in_its_name`
(`output/tests/fences.rs`) walks every crate in both directions and floors each suffix
at the count the workspace holds.
