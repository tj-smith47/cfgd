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
  - `printer.table(table)` — tabular data
  - `printer.section(name)` — returns `SectionGuard` (drop ends the section)
  - `printer.spinner(label)` — returns `Spinner` with `.finish_ok(subject)` / `.finish_fail(subject).detail(e)`
  - `printer.progress_bar(...)` — returns `ProgressBar`
  - `printer.run(cmd, fmt)` — buffered command execution with live output
  - `printer.data_line(text)` — raw structured-output line
  - `printer.emit(doc)` — `Doc` emit (for `-o json|yaml|jsonpath|template`)

**Every module receives a `&Printer` (or `Arc<Printer>` in async contexts). This is non-negotiable.**

**Status subjects must not contain `\n`.** When formatting a captured error (`io::Error`, `CfgdError`, command stderr) into a `status[_simple]` subject or detail, route through `cfgd_core::output::collapse_to_subject_line(err)` to flatten multi-line errors safely — the `Renderer::write_line` `debug_assert` will panic in debug builds otherwise.

For a user-authored script body (a `run:` entry, an `--add-*-script`/`--remove-*-script` CLI value) landing in a status subject, route through `cfgd_core::output::condense_script_label(body)` instead: it trims each line, drops empties, and truncates the first line to 80 chars with `…` (or appends ` …` if more lines follow) — a lossy, DISPLAY-only summary appropriate for "Added script: …" / "Removed script: …" confirmations. Never use it for:
- **persisted / machine-matched strings** (a resource-id, journal `resource_id`, `ActionResult.description`, a `-o json` payload field) — these must stay byte-identical to the raw body so state-matching and `-o json` consumers never see a reshaped id
- **pre-approval security-review contexts** (a module `add`/`upgrade` diff, `print_module_review_summary`) — the user must see the FULL script before approving it runs on their machine; render via `bullet()` for a single logical line or `code_block()` for a multi-line body instead of truncating
- **"not found" echoes of a user-typed search argument** — prefer `collapse_to_subject_line` there too, since hiding the tail of the exact string that failed to match defeats the point of the error

Forbidden outside the `output/` module itself:
- `println!`, `eprintln!`, `print!`, `eprint!`
- `console::*` direct use
- `indicatif::ProgressBar::new` or `MultiProgress::new` directly
- `log::*` macros — use `tracing::*` instead
- The following method names are reserved-banned (the audit gate in `.claude/scripts/audit.sh` rejects them outside `output/` itself): `success`, `warning`, `info`, `error`, `header`, `subheader`, `key_value`, `newline`, `plan_phase`, `stdout_line`.

See Hard Rule #1 in `hard-rules.md`.

## Structured-output coverage

Every `cmd_*` function in `crates/cfgd/src/cli/` must have a row in
`.claude/rules/structured-output-coverage.md`; `.claude/scripts/audit.sh`
fails when one is missing.
