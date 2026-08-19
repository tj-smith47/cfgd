---
paths: ["crates/**/*.rs"]
---
# cfgd Output System — critical design constraint

The `output` module (`crates/cfgd-core/src/output/`) provides:
- `Printer` struct: the sole interface for writing to the terminal
- Methods:
  - `printer.heading(text)` — top-level title
  - `printer.heading_title(&TitleLabel)` — top-level `Label: value` title (`Status: dev-tools`), styled through `TitleLabel`'s 3 slots (label / colon / value) instead of `heading`'s single `theme.header` coat. `Doc::heading_title(label, value)` is the structured builder entry point; a heading with no value part stays a plain `heading(...)`/`Doc::heading(...)`
  - `printer.kv(key, value)` — single key/value pair
  - `printer.kv_block(pairs)` — multi-pair block
  - `printer.status_simple(role, subject)` — concise status line; `role: Role::{Ok, Info, Warn, Fail, Skipped, Pending, Running, Accent, Secondary}`. `Accent` = "attention without alarm" (orange-family); `Secondary` = "structural pivot / label / identifier" (pink/magenta-family). Both have no icon and are suppressed at `Verbosity::Quiet` like every non-`Fail` role.
  - `printer.status(role, subject)` — returns `StatusBuilder` for `.detail(...)`, `.duration(...)`, `.label(label_role, label_text)`, `.drift(expected, actual)`, `.with_data(...)`. The `.label(...)` form appends a styled label at end-of-subject (enforced by API construction — see `compose_subject_with_label`). The `.drift(...)` form is `.detail(cfgd_core::output::drift_detail(expected, actual))` — the ONE canonical `want: <expected>, have: <actual>` spelling of a planned-vs-actual mismatch, so it can never be baked into the subject by hand. `doc::StatusFields::drift(...)` is the same composer for the buffered `Doc` path (`SectionBuilder::status_with`).
  - `printer.hint(text)`, `printer.note(text)` — supplementary output
  - `printer.deprecation(text)` — a notice that the SPELLING the user reached for is on the way out (a legacy flag or filter pattern). Always visible: it survives the structured-output auto-quiet, and writes to stderr only so the `-o` data channel stays pure
  - `printer.alert(text)` — a persistent advisory about what THIS run will actually do, when acting on the output without it would mean acting on a wrong picture (a `--skip` that stranded package installs). Same always-visible stderr routing as `deprecation`; separate because a deprecation is about spelling and an alert is about effect. Not a substitute for `status_simple(Role::Warn, …)`, which is the ordinary warning and is correctly suppressed under `-o json`. The ONE always-visible emit that is correct at any depth: it is called where the effect is discovered (a source-constraint bypass, mid-composition, under whatever section the command opened), so it renders at the open section's depth and never trips the top-level structural assert. `deprecation` keeps that assert, because a deprecation is drained at the command boundary that owns the terminal
  - `printer.table(table)` — tabular data
  - `printer.section(name)` — returns `SectionGuard` (drop ends the section)
  - `printer.spinner(label)` — returns `Spinner` with `.finish_ok(subject)` / `.finish_fail(subject).detail(e)` / `.set_message(m)` (`&mut self`, clamped via `clamp_label`, becomes the label `Drop` settles with). Abandoned without an explicit finish (an early `?` between creation and the matching `finish_ok`/`finish_fail`), `Drop` settles it as `Role::Skipped` + `" (interrupted)"` — Drop cannot know whether the in-flight work succeeded or failed, so this is the one honest record, distinct from both settled roles and from the animated running frame. Suppressed at `Verbosity::Quiet` exactly like the running spinner it replaces. A spinner borrowed from a `LiveRow` (its `.window(subject)`) is exempt: the row settles its own line, and Drop would double it
  - `printer.progress_bar(...)` — returns `ProgressBar` with `.inc(delta)`, `.set_position(pos)`, `.set_message(m)` (`&mut self`, same clamp-and-remember-for-Drop contract as `Spinner::set_message`), `.finish(self)`. `Drop` parity with `Spinner`: abandoned without `.finish()`, it settles `Role::Skipped` + `" (interrupted)"` at its label instead of leaving its last paint on screen forever. Every step of a loop driving one bar across many items (`crates/cfgd/src/files/apply.rs`'s "Applying files") must route its own fallible work through an inner fn/match-once and call `.finish()` on EVERY exit path (success and failure) rather than let an early `?` abandon the bar to Drop — the same LEAK-site discipline `Spinner` callers already follow
  - `printer.live_row_at(depth)` / `printer.live_row_after(depth, &row)` / `printer.live_row_first(depth)` — return a `LiveRow`, ONE line of the live region the CALLER owns for its whole life and rewrites in place: `set_action_status(&RowStatus, column)` (a pending or settled tree line), `window(subject)` (running, with the child's output tailing below it), `set_owner_label(&label)` (a group heading), `set_note(text)` (a muted `… ` line about the region itself), and `retire()` to take the line down. `retire` ERASES the line — it does not commit it; the permanent line is written separately into a `SectionGuard` (`reconciler::apply::emit_action_line`) and the row is retired once it has been. The erase is the row's `Drop`, which `retire` is a named call to, so a row that ends any other way — an abandoned handle, an unwind — takes its line with it too. Its order is load-bearing: the bar is CLEARED while it is still in the `MultiProgress` and removed after, because removal hides the bar's draw target and a clear issued after it paints nothing, leaving the last live paint on the terminal for whatever writes next to land beneath. `live_row_after` inserts directly beneath an existing row, which is what keeps one group's rows contiguous while another group is still growing; `live_row_first` inserts at the top, the one slot an over-full region never truncates. `printer.live_row_budget()` reports how many ROWS the region can hold before the terminal's height truncates it from the FOOT — spend the budget on rows that have nothing left to say (pending work, and settled rows whose outcome is already held for commit); NEVER retire a running row's line, or the region hides exactly the work the reader is waiting on. It is a row count, not a count of the terminal lines indicatif ends up painting: a running row tails its child's output below itself, a subject may carry `\n` continuations, and either can soft-wrap. `LIVE_REGION_HEADROOM` is the slack that covers the difference, so a caller keeps its OWN rows inside the budget and does not claim the region can never overflow
  - `printer.run(cmd, fmt)` — buffered command execution with live output
  - `printer.data_line(text)` — raw structured-output line
  - `printer.emit(doc)` — `Doc` emit (for `-o json|yaml|jsonpath|template`)

**An error `Doc` under a selector format always echoes to stderr first.** `emit_structured`
(`crates/cfgd-core/src/output/structured.rs`) routes `-o name` / `jsonpath=` / `template=` /
`template-file=` through the reader's SUCCESS-shaped selector; an error doc's shape
(`error`/`message`/`name`) almost never satisfies one written for `.items[].foo`, so — unlike
`json`/`yaml`, which dump the whole payload regardless of selector — a non-matching selector on
an error doc used to print nothing to stdout and nothing anywhere else, leaving only the exit
code to say a failure happened. `emit_structured` now writes `doc.error_message()` to
`sink_stderr` unconditionally, before evaluating the selector, whenever `doc.is_error` and the
format is one of those four. The selector still runs afterward and may separately match
something in the error doc's own fields (e.g. `jsonpath={.name}` against a `not_found` error) and
print that to stdout — so a selector format can render an error twice: once as the guaranteed
stderr diagnostic, once as whatever stdout the selector produced. Documented in
`docs/cli-reference.md`'s "Error output" section; keep the two in sync.

**Every module receives a `&Printer` (or `Arc<Printer>` in async contexts). This is non-negotiable.**

**Collapse a captured error before it becomes a status subject.** When formatting an `io::Error`, `CfgdError`, or command stderr into a `status[_simple]` subject or detail, route through `cfgd_core::output::collapse_to_subject_line(err)`: an error's own line breaks are an artifact of how it was captured, not structure the reader wants, and a one-line subject is what scans.

A subject that is *genuinely* multi-line — a brew caveat is two sentences — may carry `\n`. The renderer lays those out as continuations of the status line, indented to the marker column by `renderer::wrap::wrap_body`, so they read as part of the line they belong to rather than as unmarked lines at column 0. Never hand-roll that indent at a call site; the renderer is the single layout authority and a `\n` continuation must look identical to a soft-wrapped one.

For a user-authored script body (a `run:` entry, an `--add-*-script`/`--remove-*-script` CLI value) landing in a status subject, route through `cfgd_core::output::condense_script_label(body)` instead: it trims each line, drops empties, and truncates the first line to 80 chars with `…` (or appends ` …` if more lines follow) — a lossy, DISPLAY-only summary appropriate for "Added script: …" / "Removed script: …" confirmations. Never use it for:
- **persisted / machine-matched strings** (a resource-id, journal `resource_id`, `ActionResult.description`, a `-o json` payload field) — these must stay byte-identical to the raw body so state-matching and `-o json` consumers never see a reshaped id
- **pre-approval security-review contexts** (a module `add`/`upgrade` diff, `print_module_review_summary`) — the user must see the FULL script before approving it runs on their machine; render via `bullet()` for a single logical line or `code_block()` for a multi-line body instead of truncating
- **"not found" echoes of a user-typed search argument** — prefer `collapse_to_subject_line` there too, since hiding the tail of the exact string that failed to match defeats the point of the error

When you are holding an `Action` and its already-formatted description (apply/plan/daemon display paths), call `cfgd_core::reconciler::condense_action_desc_for_display(action, desc)` rather than deciding per call site: it applies `condense_script_label` to exactly the two arms that embed a raw script body and passes everything else through. Cataloged in `shared-utils.md`.

Forbidden outside the `output/` module itself:
- `println!`, `eprintln!`, `print!`, `eprint!`
- `console::*` direct use
- `indicatif::ProgressBar::new` or `MultiProgress::new` directly
- `log::*` macros — use `tracing::*` instead
- The following method names are reserved-banned (the audit gate in `.claude/scripts/audit.sh` rejects them outside `output/` itself): `success`, `warning`, `info`, `error`, `header`, `subheader`, `key_value`, `newline`, `plan_phase`, `stdout_line`.

See Hard Rule #1 in `hard-rules.md`.

## Provider narration goes to the note sink, never to the printer

A `PackageManager` or `SystemConfigurator` executes UNDER an action line the reconciler
settles from the plan. A `status_simple` called from inside one of them therefore lands
*above* the line describing the same work, outside the phase tree. Both traits carry a
context whose `report` is the narration channel:

```rust
// PackageManager — the tag names the speaker, because the action line names the package
cx.report(Role::Warn, self.name(), "brew: run `brew link --force`");

// SystemConfigurator — no tag: the action line already reads system:<name>.<key>
cx.report(Role::Info, format!("systemctl {action} {name}"));
```

Every note reported this way collects into the run's `ApplyResult.caveats` — grouped by
the `kind:name` owner that produced it — and renders exactly once, as a single closing
`Caveats` section printed after the run's summary line, instead of attached under the
action that produced it:

```
✓ set sysctl.net.ipv4.ip_forward: 0 → 1     ← the reconciler's line, from the plan

✓ Apply complete — 1 action succeeded (0.1s)

Caveats
  profile:work
    ⚠ reload deferred: /proc is read-only     ← Warn renders before Info within a group
    ⊙ sysctl -w net.ipv4.ip_forward=1
```

Both land in one `NoteSink` and route through one rule (`NoteSink::report_tagged`), then
through one collection point — `Reconciler::settle_action`, called from both the
concurrent-lane dispatch and the serial dispatch loop, is the ONE place a settled action's
notes are folded into the run's `caveats` collector via `collect_caveats` — and one render
path, `cfgd_core::reconciler::render_caveats` (opened through `Printer::section_caveats`,
the "Caveats" heading painted `theme.accent`, unstyled of bold — the phase-name slot, no
`.bold()` since R12 forbids pairing bold with a colour-bearing slot). `cli::plan_ops::print_caveats` is the one assembler for a real `cfgd apply`
(it also folds the `cfgd:env` re-source reminder into that owner's group, always last); a
per-configurator snapshot bridge is the only other caller. Never grow a second drain or a
second render path. A context nobody drains (`SystemContext::new`, `PackageContext::new`,
`NoteSink::discarded()`) settles the report on the printer instead, so a standalone caller
loses nothing.

`SystemContext`'s fields are private: `report` and `run_silent` are the whole surface, so
`cx.printer.status_simple` is not expressible rather than merely discouraged. Never add a
`printer()` accessor. A snapshot bridge that drives a configurator directly renders through
`render_caveats` after its own closing summary, so its golden pins the real run-wide shape
production emits rather than one the test assembled.

## Source-constraint mode (every `compose_with_sources` call site)

**`ConstraintMode::Report` is for read paths; every path that mutates the machine composes in `Enforce`.** Decide on what the command *does*, not on what it reads: `backup run` reads config like `status` but executes hooks and writes snapshots, so it is `Enforce`. `Report` records a source violation and continues (the read still has to render); `Enforce` aborts on the first one.

| Mode | Commands |
|---|---|
| `Report` | `status`, `diff`, `verify`, `compliance *`, `backup list`, `checkin`, `decide` — anything whose whole job is to describe state (`decide`'s composition is a classification READ; its write is a decision-store row, never a change to the machine, and `Enforce` would disable answering exactly when a source violates a constraint) |
| `Enforce` | `apply`, `plan`, `daemon`, `backup run`, `backup restore`, `source add` — anything that runs a script, writes a file, or takes a snapshot |

`Report` is not "skip the check": `compose` still warns per violation, and any script surface a
read path would EXECUTE is marked unrunnable in the composed spec (`composition::block_barred_scripts`
poisons a barred source's `patch.script`, so evaluating the file degrades instead of running it).
Adding a script surface that a `Report`-mode command evaluates means extending that marking too —
a surface only `Enforce` reaches needs nothing.

## Structured-output coverage

Every `cmd_*` function in `crates/cfgd/src/cli/` must have a row in
`.claude/rules/structured-output-coverage.md`; `.claude/scripts/audit.sh`
fails when one is missing.

## No `tracing::info!`/`warn!`/`error!` in the config/module/source domains

Banned anywhere under `crates/cfgd-core/src/config/`,
`crates/cfgd-core/src/modules/`, or `crates/cfgd-core/src/sources/` — the three
domains whose whole job is turning user-authored YAML/TOML into cfgd's typed
config. `tracing::info!`/`warn!`/`error!` writes to a channel that's invisible
without `RUST_LOG` set (and `info!` is the least visible of the three — the cfgd
binary's own default filter is `warn`); a legacy-key deprecation, an ambiguous-profile notice,
or a malformed-manifest warning routed there is an advisory the user never
sees, the exact bug `warn_on_legacy_theme_keys` shipped with before it was
rerouted (see `parse::REMOVED_THEME_KEYS` / `RENAMED_THEME_KEYS`).

Use instead: collect the message into a `Vec<String>` the caller can drain
through `printer.deprecation(text)` (or `printer.alert(text)` for a run-affecting
notice) at the command boundary that actually owns a terminal — these core
functions have many callers, none of which hold a `Printer`. `parse_config`'s
`CfgdConfig.deprecations` field (`#[serde(skip)]`, drained once per command via
`crates/cfgd/src/cli/helpers.rs`) is the working example to extend, not to
reinvent per call site.

`.claude/scripts/audit.sh` enforces this on the DOMAIN — every non-test `.rs`
under those three directories — rather than on a function-name shape. An earlier
revision anchored on a `fn parse_*` / `fn load_*` signature and walked the
function's brace span; it selected the wrong set, because `warn_on_legacy_theme_keys`
is named neither, and neither is any advisory helper a parse function calls
(`check_yaml_anchor_limit`, `read_manifest`, `validate_source_name`, …). The
domain anchor covers all of them and needs no span walk, so no string literal or
body-less trait signature can end a scan early.

Escape hatch for a genuinely internal diagnostic (one no interactive user is
meant to read) — mirrors `native-ok:` / `spawn-blocking-ok:` — mark the call
line or the comment line directly above it:

```rust
tracing::warn!("cache miss for {}", key); // tracing-ok: internal cache-timing diagnostic, not user-facing
```

The marker counts only inside a comment, only with a reason written after it,
and is inherited only from a comment line — a call cannot exempt itself by
naming the hatch in its own message string, and a marked call does not exempt
the unmarked call beneath it.

**What disqualifies a message from the hatch**, whatever the marker says: a
message describing the user's own config, a key they wrote, a migration they
have to perform, or anything that changes what they should do next is
user-facing. "Internal" means a diagnostic whose entire audience is someone
already reading `RUST_LOG` output — cache timings, retry counts, protocol
traces.

## A tracing event never restates a Printer line

**Whatever the domain: if a `Printer` already says it on this path, the tracing
event may not say it again.** The duplicate is not merely noise — it is a second
copy of the same sentence written to the ONE stream the live region repaints,
and any write there that does not go through the region strands the last paint
of whatever bar is on screen. `cfgd module push` printed its result three times
that way (spinner label, `info!`, `finish_ok` + `Digest` kv) and left the
spinner frozen on the terminal doing it.

```rust
// WRONG — the caller already prints "Signed artifact"
tracing::info!(reference = artifact_ref, "artifact signed with cosign");

// RIGHT — the fields are a debugging detail, the sentence is the Printer's
tracing::debug!(reference = artifact_ref, "artifact signed with cosign");
```

Demote to `debug!` when the event carries a field the printed line does not (a
digest, a pid, a count) — that keeps it for whoever is reading `RUST_LOG` and
takes it out of everyone else's terminal. Delete it when it carries nothing the
printed line does not.

An event that is NOT a duplicate — a genuinely internal diagnostic, an event
the daemon's journal is the only reader of — stays at the level it belongs at
and carries the `// tracing-ok: <why>` marker inside the banned domains.

**What the audit gate enforces, exactly.** `tracing::info!` is rejected in every
non-test `.rs` under `crates/cfgd-core/src` and `crates/cfgd/src` — the whole of
both crates, not the three config/module/source domains above — with one
exemption and one hatch:

- **`daemon/` is exempt at any depth in either crate.** There the log IS the
  output: a service under systemd/launchd prints its ticks to journald through
  this channel and no other, which is why `cfgd daemon run` keeps `info` as its
  tracing floor (`main.rs::runs_reconcile_loop`).
- **The `// tracing-ok: <why>` hatch applies**, read the same way as the domain
  gate's: only inside a comment, only with a reason after it, inherited only
  from the comment line directly above the call, and never from the call's own
  message string.

`warn!` and `error!` are NOT part of this gate — outside the three domains above
they stay legal, because the binary's default filter is `warn` and those levels
reach the user. `info!` is the level nobody sees without `RUST_LOG`, so an
`info!` outside the daemon is a line nobody reads AND a strand risk when they
do.

## The two mechanisms that keep tracing off the live region

One is the writer, in `output/`; the other is the default filter, which lives
where the flags are parsed — `crates/cfgd/src/main.rs`. Nothing outside `output/`
may build a `MakeWriter` of its own, and nothing but `main.rs` picks a default
filter.

- **`output::LiveTracingWriter`** (`output/tracing_writer.rs`) — the `MakeWriter`
  the cfgd binary installs on its subscriber. Every event is written through the
  printer's `MultiProgress`, so it clears the bars, lands, and lets them repaint
  beneath it. `main.rs` builds one before the subscriber (the subscriber has to
  be live for anything the printer's construction logs) and calls `attach(&printer)`
  once the process printer exists; an unattached writer, and one attached to a
  printer with no live region, writes plain stderr. Never wire a subscriber to
  `std::io::stderr` in the cfgd binary again — the `no_subscriber_writes_straight_to_stderr`
  fence rejects any `with_writer(…)` whose argument names `stderr`, in any
  spelling and across the lines rustfmt splits it onto. A writer legitimately
  NAMED for stderr (a test capture) takes the marker hatch every sibling gate
  carries: `// stderr-writer-ok: <why>` on the call line or the line above it,
  reason required.
- **`crates/cfgd/src/main.rs::tracing_filter_for(quiet, verbose, daemon)`** — the
  default filter. A command defaults to `warn`; the flags keep the meanings they
  document (`-v` = `debug`, `-vv` = `trace`) and `--quiet` is `error`, so only
  the no-flag default moved. The RECONCILE LOOP keeps `info` as its floor —
  `cfgd daemon` (bare), `cfgd daemon run` and the SCM-launched `cfgd daemon
  service`, selected by `main.rs::runs_reconcile_loop` — because there the log
  IS the output: a service prints its ticks to journald through this channel and
  no other. `daemon install` / `uninstall` / `status` are ordinary one-shot
  commands and keep the `warn` default, since they report through the `Printer`.
  `RUST_LOG` outranks all of it.

## `LiveBarState` is shared by every renderer writing one live region

`renderer::LiveBarState` (the live-bar count plus the broken-terminal latch) is
held in an `Arc` and carried through `Printer::build_derived`, because a derived
printer writes the SAME `MultiProgress` and the SAME sinks as its parent. A
derived renderer counting its own bars starts at zero, answers the routing gate
"no bar is live" while the parent's spinner is painting, and raw-writes over it
— which is what froze `cfgd sync`'s spinner, since every quiet library sink
(`cli/sync.rs`, `cli/compliance.rs`, `cli/source/show.rs`, `cli/daemon.rs`,
`daemon/sync.rs`) is a derived printer whose `Fail` statuses, `alert()`s and
`deprecation()`s survive `Verbosity::Quiet`. Never mint a fresh `LiveBarState`
for a renderer that shares an existing region.

A claim about a stranded paint is provable on ONE surface only —
`Printer::for_test_live_terminal`, the emulated screen. See `shared-utils.md`,
"three live-region capture constructors".
