---
paths: ["**/*.rs"]
---
# cfgd Output System — critical design constraint

The `output` module (`crates/cfgd-core/src/output/`) provides:
- `Printer` struct: the sole interface for writing to the terminal
- Methods:
  - `printer.heading(text)` — top-level title
  - `printer.kv(key, value)` — single key/value pair
  - `printer.kv_block(pairs)` — multi-pair block
  - `printer.status_simple(role, subject)` — concise status line; `role: Role::{Ok, Info, Warn, Fail, Skipped, Pending, Running, Accent, Secondary}`. `Accent` = "attention without alarm" (orange-family); `Secondary` = "structural pivot / label / identifier" (pink/magenta-family). Both have no icon and are suppressed at `Verbosity::Quiet` like every non-`Fail` role.
  - `printer.status(role, subject)` — returns `StatusBuilder` for `.detail(...)`, `.duration(...)`, `.label(label_role, label_text)`, `.with_data(...)`. The `.label(...)` form appends a styled label at end-of-subject (enforced by API construction — see `compose_subject_with_label`).
  - `printer.hint(text)`, `printer.note(text)` — supplementary output
  - `printer.deprecation(text)` — a notice that the SPELLING the user reached for is on the way out (a legacy flag or filter pattern). Always visible: it survives the structured-output auto-quiet, and writes to stderr only so the `-o` data channel stays pure
  - `printer.alert(text)` — a persistent advisory about what THIS run will actually do, when acting on the output without it would mean acting on a wrong picture (a `--skip` that stranded package installs). Same always-visible stderr routing as `deprecation`; separate because a deprecation is about spelling and an alert is about effect. Not a substitute for `status_simple(Role::Warn, …)`, which is the ordinary warning and is correctly suppressed under `-o json`
  - `printer.table(table)` — tabular data
  - `printer.section(name)` — returns `SectionGuard` (drop ends the section)
  - `printer.spinner(label)` — returns `Spinner` with `.finish_ok(subject)` / `.finish_fail(subject).detail(e)`
  - `printer.progress_bar(...)` — returns `ProgressBar`
  - `printer.run(cmd, fmt)` — buffered command execution with live output
  - `printer.data_line(text)` — raw structured-output line
  - `printer.emit(doc)` — `Doc` emit (for `-o json|yaml|jsonpath|template`)

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
