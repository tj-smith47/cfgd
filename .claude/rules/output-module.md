---
paths: ["crates/**/*.rs"]
---
# cfgd Output System — critical design constraint

The `output` module (`crates/cfgd-core/src/output/`) provides the `Printer` struct: the sole interface for writing to the terminal. This file is the CATALOG of that surface and the RULES for reaching it; the reasoning behind each composer lives in its own rustdoc, and a rule's story in its pinning test's doc comment. **An entry or rule row here is a few sentences**: the shape, the never-clause, at most one pin name. `.claude/scripts/audit.sh` gates every entry's and table row's byte size so this density holds.

## Printer surface

Reach for the composer matching the call site's shape; never hand-build a `format!` string a composer owns.

- `heading(text)` — top-level title.
- `heading_title(&TitleLabel)` — a `Label: value` title, styled through `TitleLabel`'s three slots; `Doc::heading_title[_typed]` are the buffered entry points. A heading with no value part stays a plain `heading`.
- `heading_owner_prefixed(prefix, &OwnerLabel)` — a `<Verb> <owner>` heading (`Add source:acme`), and the ONLY heading slot an owner token may occupy; `Doc::heading_owner_prefixed` is the buffered entry point. No unprefixed counterpart exists: an owner names WHOSE the rows below it are, which is a section's job.
- `kv(key, value)` / `kv_block(pairs)` — a key/value fact and a block of them.
- `SectionGuard::kv_rows(rows)` / `Doc::kv_rows(rows)` / `SectionBuilder::kv_rows(rows)` — `kv_block` over hand-built `KvPair`s, and how a row reaches the four renderer-owned slots: `annotated(key, value, note)`, `role_valued(key, value, role)`, `nested(key, value)` and `owner_valued(key, owners)` (the ONE way an owner token occupies a kv value). **A caller never paints or indents one itself.** All four are `#[serde(skip)]`, display-only — the plain `value` is what `-o json` reads — and why `Printer::muted` no longer exists.
- `command_list(pairs)` — a "command — description" list, `kv_block`'s counterpart for a left column that NAMES a thing rather than carrying data; canonical `" — "` glue, descriptions hang to their own column. `CommandPair::typed` paints a type span. `Doc` / `SectionBuilder` / `SectionGuard` counterparts exist.
- `Doc::paragraph(text)` — a prose paragraph: no glyph, no key column, for what a documentation surface says ABOUT the heading above it. Empty input emits nothing; buffered-`Doc` only.
- `status_simple(role, subject)` — a concise status line. `Role::{Ok, Info, Warn, Fail, Skipped, Pending, Running, Accent, Secondary}`; `Accent` is "attention without alarm", `Secondary` a structural pivot/label. Both are iconless and suppressed at `Verbosity::Quiet` like every non-`Fail` role.
- `status(role, subject)` — a `StatusBuilder` for `.detail`, `.duration`, `.label(role, text)`, `.qualifier(text)`, `.drift(expected, actual)`, `.with_data`. `.qualifier` composes `subject: qualifier` as THREE slots; `.drift` is the ONE canonical `want: X, have: Y` spelling. `doc::StatusFields` carries both for the buffered path, via `status_with`.
- `StatusFields::verdict(word)` (buffered `Doc` only) — a verdict-led detail: the renderer paints the word with the row's ROLE and renders `.detail` as the muted parenthetical (`— Synced (1 file)`); a caller never paints the word — `cursor_safe` eats a caller's coat. `Doc::section_annotated(name, note, build)` is the heading twin, a renderer-owned muted `(note)` on a section head. `component_health_lists_every_owner_with_a_themed_verdict` pins both.
- `hint(text)` / `note(text)` — supplementary output.
- `hint_commands(prose, &[&str])` — a hint whose colon-introduced payload is commands, each on its own indented `$ ` line; the renderer owns the indent and the prompt, so a call site supplies bare commands. `SectionGuard` / `Doc` counterparts exist, and `HintCommands` is what a hint COMPOSER returns. `every_hint_command_block_line_comes_from_the_one_composer` pins it.
- `deprecation(text)` — the SPELLING the user reached for is on the way out. Always visible, stderr-only so the `-o` channel stays pure; drained at the command boundary.
- `alert(text)` — a persistent advisory about what THIS run will actually do (effect, where `deprecation` is spelling); not a substitute for `status_simple(Role::Warn, …)`, correctly suppressed under `-o json`. The ONE always-visible emit correct at any depth.
- `table(table)` — tabular data.
- `section(name)` — a `SectionGuard` (drop ends it) carrying `bullet`/`kv`/`kv_block`/`command_list`/`hint`/`note`/`table`/`code_block`/`child_row` plus `.diff` and `.syntax_highlight`. A nested plain section paints its heading `theme.secondary`, so nesting reads from styling alone.
- `section_title(&TitleLabel)` — a `Label: value` section head (`Restore: notes`), the only titled heading that can carry a block of rows; a heading plus a top-level `kv_block` reads as the command's output rather than facts about the run above.
- `section_owner(&OwnerLabel)` / `section_owner_or_collapse(&OwnerLabel)` — a top-level section headed by a styled owner token; `_or_collapse` leaves no trace when nothing renders inside it. `SectionGuard` carries the nested pair, `Doc::section_owner` / `subsection_owner` the buffered ones. A `Doc` belonging wholly to one owner opens with `Doc::section_owner` — the owner is never the `Doc`'s heading.
- `diff(old, new)` / `syntax_highlight(code, lang)` — nest at whatever depth is ambient, flushing any pending section header first.
- `spinner(label)` — a `Spinner` with `.finish_ok` / `.finish_fail(...).detail(e)` / `.set_message`. Abandoned without an explicit finish, `Drop` settles it `Role::Skipped` + `" (interrupted)"`; a spinner borrowed from a `LiveRow` is exempt, the row settling its own line.
- `narrate(running, |sp| …)` — the settle-safe wrapper, for a long wait whose failure NOBODY ELSE reports: success retires the bar silently (goldens stay goldens), failure settles `Role::Fail` at the last `set_message`. **Reach for this instead of a hand-rolled spinner + match at any site whose body can `?`.** A wait inside a command deriving a Quiet printer is narrated through the OWNING printer.
- `narrate_silent(running, |sp| …)` — the same for a wait whose OUTCOME LINE belongs to somebody else. The criterion is who else SAYS the failure, never how the `Err` travels.
- `Spinner::finish_silent()` — retire a bar printing nothing; only where neither wrapper fits (an `output/` internal, or no printer to call a wrapper on).
- `progress_bar(...)` — `.inc`, `.set_position`, `.set_message`, `.finish(self)`, `Drop` parity with `Spinner`. A loop driving one bar across many items routes its fallible work through an inner fn and calls `.finish()` on EVERY exit path.
- `live_row_at(depth)` / `live_row_after(depth, &row)` / `live_row_first(depth)` — a `LiveRow`, ONE line of the live region the CALLER owns and rewrites in place. `retire` ERASES rather than commits; the permanent line goes separately into a `SectionGuard`. `live_row_budget()` is a ROW count — spend it on rows with nothing left to say, and NEVER retire a running row.
- `run(cmd, fmt)` — buffered command execution with live output.
- `data_line(text)` — a raw structured-output line.
- `emit(doc)` — `Doc` emit (for `-o json|yaml|jsonpath|template`).

## One titled section per command — a result section never respells the title

A command's output is headed ONCE, by the thing the command is: the title IS its section, opened with `printer.section("<Verb> <Noun>")` plus `let _inherit = printer.depth_inheritance();`, and the header facts, result rows and every owner sub-section go inside it. A second section under that title may not respell any word the title spent — `Push Module` → `Push` is banned, `Sync` → `Sources` is fine, because a sub-section names a DIFFERENT subject.

```
BAD                                   GOOD
────────────────────────────────      ────────────────────────────────
Push Module                           Push Module
  Directory: ./mod                      Directory: ./mod
  Push                                  ✓ Pushed module
    ✓ Pushed module                     Digest: sha256:…
```

`no_result_section_respells_a_word_its_command_title_already_spent` walks every `heading`/`section` literal pair under `crates/cfgd/src/cli/`.

## Wording rules every closing line and hint obeys

Eight conventions, each with a walk-the-population pin (in `crates/cfgd/src/cli/tests.rs` unless noted) that fails on the next member that breaks it.

| Rule | Shape | Pin |
|---|---|---|
| A result line is **sentence case, past-tense verb first, count after** | `✓ Accepted 1 item`, `✓ Installed daemon service` — never `Daemon service installed`, never Title Case. Partitions the sources with the body-row pin: a subject slot under `reconciler/`/`backup/` is a body row, everything else is a result line | `every_result_line_is_sentence_case` |
| A message naming a command **quotes it in backticks** | ``Run `cfgd decide accept <resource>` to answer`` — never `'cfgd …'`; covers hints, errors and clap help, both crates | `every_command_a_message_names_is_quoted_in_backticks` |
| The **up-to-date verdict is not a command's to word** | every no-actions verdict settles through `reconciler::nothing_to_do_verdict(pending)`, which withholds `Ok` while decisions are pending | `no_command_words_the_up_to_date_verdict_for_itself` |
| A **rendered label is Title Case**, whichever slot holds it | kv key, `KvPair`, row tuple and table header all read `Last Sync`; small words stay lowercase off the front; a label NAMING a thing keeps its spelling under `// name-row-ok:` | `every_rendered_label_is_title_case` |
| A **`source` verdict is counted iff the verb takes many subjects**, and every mutating verb **closes its success path on a next step** | `✓ Updated 1 source` beside bare `✓ Subscribed`; each followed by `success_next_step`'s hint (see `shared-utils.md`). A workflow-ending verb is hatched in the walk's table with its reason | `every_source_verdict_counts_iff_its_verb_takes_many_subjects`, `every_mutating_verb_closes_on_a_next_step` |
| A **count belongs to its section's annotation**, not to a row | `Pending Decisions (1 item)` via `reconciler::pending_decisions_title`, never a `⊙ 1 pending item` row | `pending_decisions_title`'s unit tests + goldens |
| A **section's instruction closes it from inside** | the answer hint under a decisions section renders at the section's depth straight under its last row, never a `Doc::hint` hung off the document; exactly two composers emit it | `every_decisions_hint_closes_its_section_from_inside` |
| The **closing line holds one em-dash and one trailing parenthetical** | `✓ Apply complete — 21 actions succeeded, 1 not attempted: no session manager (278.2s wall)` — the em-dash joins title to detail, the withheld clause comes last with its reason after a colon, the one `(…)` is the elapsed. `outcome_counts` is the ONE composer | `the_closing_line_holds_one_em_dash_and_one_trailing_parenthetical` (`reconciler/run/tests.rs`) |
| A hint's **colon-introduced command drops to a `$` block** | inline backticks stay for one short command named mid-sentence; a colon-introduced payload, or more than one command, goes to `hint_commands`. Two alternatives of the SAME command differing in one token collapse to one `[a\|b]` line; distinct commands keep their own lines. `MSG_RUN_APPLY`, `backup::safety_copy_hint` and the `helpers.rs` profile-not-found flow stay as they are by ruling. A `` : ` `` left in a hint's own text is the walk's tell | `every_hint_command_block_line_comes_from_the_one_composer` |
| A **closing hint names the command that comes next**, in backticks | ``Run `cfgd apply` to …`` — never "changes take effect on next reconcile". A hint built elsewhere is pinned by its producer and registered on the walk's `PINNED_HINT_COMPOSERS` allowlist; a genuinely command-less instruction carries `// hint-ok: <why>`. A run's non-`Success` verdict closes through `reconciler::run_next_step`, and a verdict line states facts only | `every_closing_hint_names_a_command`, `every_unfinished_verdict_closes_on_the_one_next_step` (`reconciler/run/tests.rs`) |

## Rendering rules every action row obeys

Seven conventions about the SHAPE of a settled row, each pinned (in `crates/cfgd-core/src/` unless noted). A row is painted by three surfaces and settled by two trees, so every rule is enforced at the ONE seam all five read, never at a call site.

| Rule | Shape | Pin |
|---|---|---|
| A **withheld row holds its subject back and lets the reason speak** | `Pending`/`Skipped` render the subject muted and the detail bright; every other role the reverse. Both halves answer from `renderer::{action_subject_style, action_detail_is_muted}` | `both_trees_paint_a_withheld_row_with_the_same_bytes` (`reconciler/run/tests.rs`) |
| **One alignment column per report**, not per phase — and every detail-bearing ROW SHAPE pads to it | measured once over every action the run will print, claimed through `Printer::report_column`, pseudo-phases included; a bullet's trailing detail pads to the same claim through `Emitting::bullet_column`, a deploy's per-file child through `Emitting::child_row_column`. The claim, the trailing allowance and the subject budget are one computation (see `shared-utils.md`, `report_align_width`, which folds a child's own effective width in too); a too-wide subject or child glues instead, and neither widens nor withdraws the column | `every_detail_bearing_row_of_a_report_lands_in_the_reports_one_column` (`reconciler/run/tests.rs`) |
| A **pre-skipped action is priced by ONE predicate at both ends** | `Action::pre_skip_reason` keeps it out of `Actions N planned` AND out of the counted rollup; its row keeps the reason, the verdict names it as the last clause — never as a `skipped` that ran | `a_pre_skipped_action_is_priced_outside_the_counted_rollup` (`reconciler/run/tests.rs`) |
| A **wrapped row lands its duration at the group's column, never at the terminal edge** | the group's settled column is threaded through to `wrap::wrap_body_with_trailer`: a last line short of it pads out, one past it glues inline, a group with no column glues on every row | `a_wrapped_row_in_a_group_with_a_column_pads_its_last_line_to_that_column` (`output/renderer/wrap.rs`) |
| A **run's total is wall-clock and says so; a row's span never does** | the rollup composes ` (278.2s wall)` through `Elapsed::wall`; every row, spinner finish and window takes `Elapsed::row`. Lanes run concurrently, so rows may sum past the total — the word is the explanation | `a_wall_clock_total_says_wall_and_a_row_span_does_not` (`output/renderer/status.rs`) |
| An **action row's subject opens on a lowercase verb** | `create ~/.zshrc`, `brew install jq`, `snapshot notes.md.<stamp>` — the row names WORK and the glyph says how it went; sentence-case past tense is the closing line's grammar. A subject opening on a proper noun takes `// name-row-ok:` | `every_action_row_subject_opens_on_a_lowercase_verb` (`crates/cfgd/src/cli/tests.rs`) |
| A **duration slot never renders a zero** | anything rounding to `0.0` renders ` (<0.1s)` — a sub-tick action DID run. One composer (`renderer::status::duration_text`) feeds every duration slot | `a_sub_tick_duration_renders_the_floor_and_never_a_zero` (`output/renderer/status.rs`) |

### The grammar split, stated once

Three grammars share one screen, and which one a string takes is decided by WHAT the string is, never by which surface prints it:

| Kind | Grammar | Example |
|---|---|---|
| **Body row of a run** — previewed by the plan and settled by the apply, ONE string in both slots | lowercase imperative | `create ~/.zshrc`, `provision npm via apt (rustc)` |
| **Result line** — an outcome reported once, never previewed | sentence case, past-tense verb first | `✓ Installed daemon service`, `✓ Cloned repository` |
| **Provider note** — a `.report(` body under a settled row | a sentence, or the command as it was run | `Updated /etc/environment`, `systemctl restart foo` |

Headings are not in any of the three: a heading is a Title Case label. A note echoing a command keeps the command's own spelling, which is why the note population is judged on its ROLE, not its case. Both snapshot normalizers know the duration floor's spelling, so goldens stay host-stable.

## A run's two ends come from one skeleton

**A command that closes with the shared rollup opens with the shared header.** Both ends come from `reconciler::ApplyRun`; a run whose body the caller renders builds one through `ApplyRun::unplanned(ctx, actions)` — never a synthesized empty `Plan`, never a second header renderer (`output::config_header_rows` is the ONE builder of the four header rows). `RunContext::subject` puts the acted-on unit in the title, `RunContext::unit_source` its declared path in the header — an INPUT fact, so it heads the run and renders BEFORE any confirmation prompt. `every_run_that_renders_the_rollup_also_renders_the_run_header` walks both crates (`// run-header-ok: <why>` where the caller already rendered one).

**A run's only phase prints no phase row.** A plan-less `ApplyRun` renders its backups through `reconciler::sole_phase` (owner groups at the run's own depth); a run carrying a `Plan` keeps `pseudo_phase(BACKUPS_PHASE_LABEL)`. `a_backup_run_prints_no_phase_row_for_its_only_phase` (`backup/tests.rs`) pins the split.

## Blank lines: the composer owns the separator

**One blank line between sibling blocks; none at the start, none at the end, none after a heading that owns rows, never two in a row.** Every blank line is a producer's, never a call site's: a top-level section close and a top-level group boundary each ARM one pending blank, and the next line drains it. A streamed child line consumes the pending blank and re-arms the boundary, so a finish line still binds to what it announced. `data_line` writes a payload as ONE line.

A title that owns no rows of its own (`Doctor`, `Verify`) is a DOCUMENT title; a titled RUN (`Diff: nvim`) carries its header rows. A call site never emits a bare blank line; a missing blank names a composer that cleared the boundary without re-arming it. `every_golden_separates_sibling_blocks_with_one_blank_line` walks every golden under both crates.

## Rules every key/value block and shared table obeys

| Rule | Shape | Pin |
|---|---|---|
| A **command's produced facts are its rows' details, never rows between them** | INPUT facts are ONE kv block before the action rows; a fact a step PRODUCES is that step's row detail (`✓ Pushed module — sha256:69c3…`) or a closing kv block after the LAST row. A kv row never sits between two result lines; a branch printing a kv INSTEAD of a status carries `// facts-block-ok: <why>`. The action-row half is produced once by `reconciler::action_produced_detail` | `no_kv_row_sits_between_two_result_lines` (`crates/cfgd/src/cli/tests.rs`) |
| **One key column per section**, whatever else the section printed between the blocks | the width is carried on the open `SectionFrame` rather than computed at close — the earlier rows are already on the terminal | `a_section_keeps_one_key_column_across_everything_it_prints` (`output/renderer/kv.rs`) |
| A **kv value never hand-builds the annotation slot** | `KvPair::annotated(key, value, note)` owns the muted `value (note)` — never `KvPair::new("Source", "remote (locked)")`, which hides the note from anything reading the value | `no_kv_value_hand_builds_the_annotation_slot` (`crates/cfgd/src/cli/tests.rs`) |
| A **kv value that is a link never hand-builds the fallback** | `KvPair::linked(key, text, url)` owns the OSC 8 slot: `text` where the theme carries hyperlinks, the `url` ITSELF everywhere else; the capability is detected by the two production constructors alone and gated by colour | `every_docs_pointer_the_cli_renders_goes_through_the_linked_slot` (`crates/cfgd/src/cli/tests.rs`) |
| A table **drops a column no row can fill** | every `Table` the CLI renders settles through `Table::without_unfillable_columns`; the absence token is the ONE `cfgd_core::ABSENT`, which is what lets one predicate judge a column; `-o json` keeps every field | `every_listing_the_cli_renders_drops_a_column_no_row_can_fill` (`crates/cfgd/src/cli/tests.rs`) |
| Every surface listing config sources renders **the one `Sources` table** | `source list`, `status` and `daemon status` all build through `source::list::sources_table` under `SOURCES_SECTION`; a surface holding live facts merges them OVER a catalog row | `both_sources_surfaces_render_through_the_one_table_builder` (`crates/cfgd/src/cli/tests.rs`) |

## The reconcile loop has no printer to report to

A daemon under systemd/launchd is read through its journal, so the loop's own account of itself goes to `tracing` and NOTHING to the `Printer` — a printed duplicate is a second copy on the stream the live region repaints. `daemon/service/` is the exception at any depth: install/uninstall are one-shot commands watched from a terminal. `every_daemon_info_event_names_its_subsystem` (`output/tests/fences.rs`) walks the loop's files.

## Sanitizing text cfgd did not author

`cursor_safe` (`output/mod.rs`) is the ONE renderer FOLD, and it covers every slot above that carries caller text. **A call site echoing a gateway field, a remote source's description or a tool's captured stderr through one of those slots does NOT sanitize it by hand.**

Two other policies exist and are not interchangeable with it: a PRE-APPROVAL surface ESCAPES (a screen the operator approves from has to SHOW it), and `prompt_text`'s `default` STRIPS (a default is returned AS the answer). Every payload — a `plain()` form, a persisted string, `-o json`, `data_line` — stays byte-exact.

**The full routed-slot inventory, the escape/strip surfaces and the terminal writers that take no policy are in `output/mod.rs`'s module doc.** A new slot rendering caller text routes through `cursor_safe` and is added there.

**An error `Doc` under a selector format always echoes to stderr first.** A success-shaped selector almost never matches an error doc, so `doc.error_message()` reaches `sink_stderr` unconditionally before the selector runs (`output/structured.rs`); kept in sync with `docs/cli-reference.md`'s "Error output".

**Every module receives a `&Printer` (or `Arc<Printer>` in async contexts). This is non-negotiable.**

## Collapsing an error or a script body into a subject

**Collapse a captured error before it becomes a status subject**: route it through `cfgd_core::output::collapse_to_subject_line(err)` — an error's line breaks are an artifact of capture. A genuinely multi-line subject may carry `\n`; the renderer lays continuations out, never hand-roll the indent.

**A DETAIL takes `cfgd_core::output::captured_output_detail(err)` instead** — same bound, no flattening. Both folds cap at the live window's `VISIBLE_LINES` and elide from the middle. Display-only: the journal, `ActionResult.error` and `-o json` keep the full text.

For a user-authored script body landing in a subject, route through `cfgd_core::output::condense_script_label(body)` — a lossy, DISPLAY-only summary. Never use it for: persisted / machine-matched strings; pre-approval security-review contexts (the user must see the FULL script — use `bullet()` or `code_block()`); "not found" echoes of a user-typed search argument (prefer `collapse_to_subject_line`). Holding an `Action` and its formatted description, call `reconciler::condense_action_desc_for_display(action, desc)` rather than deciding per call site.

## Forbidden outside the `output/` module

- `println!`, `eprintln!`, `print!`, `eprint!`
- `console::*` direct use
- `indicatif::ProgressBar::new` / `MultiProgress::new` directly
- `log::*` macros — use `tracing::*`
- These method names are reserved-banned (rejected by `.claude/scripts/audit.sh` outside `output/`): `success`, `warning`, `info`, `error`, `header`, `subheader`, `key_value`, `newline`, `plan_phase`, `stdout_line`.

See Hard Rule #1 in `hard-rules.md`.

## Provider narration goes to the note sink, never to the printer

A `PackageManager` or `SystemConfigurator` executes UNDER an action line the reconciler settles from the plan, so a `status_simple` from inside one lands outside the phase tree. Both traits carry a context whose `report` is the narration channel:

```rust
// PackageManager — the tag names the speaker, because the action line names the package
cx.report(Role::Warn, self.name(), "brew: run `brew link --force`");

// SystemConfigurator — no tag: the action line already reads system:<name>.<key>
cx.report(Role::Info, format!("systemctl {action} {name}"));
```

Every note collects into `ApplyResult.caveats`, grouped by the `kind:name` owner that produced it, and renders once as a closing `Caveats` section:

```
✓ set sysctl.net.ipv4.ip_forward: 0 → 1     ← the reconciler's line, from the plan

✓ Apply complete — 1 action succeeded (0.1s)

Caveats
  profile:work
    ⚠ reload deferred: /proc is read-only     ← Warn renders before Info within a group
    ◉ sysctl -w net.ipv4.ip_forward=1
```

**A note's role answers ONE question — must the reader act?** A caveat or degraded fallback is `Role::Warn`; a report of work done on the side is `Role::Info`; an instruction goes through `SystemContext::next_step` / `NoteSink::next_step` and renders as a `→` hint; nothing is `Role::Ok`. Brew's `==> Caveats` is a SECTION, not a severity, so a brew body is classified by `packages::shared::brew_caveat_asks_the_reader_to_act`. `every_provider_note_takes_its_role_from_whether_the_reader_must_act` walks every `.report(` literal under `packages/**` and `system/**`.

Both land in one `NoteSink` and route through one rule (`NoteSink::report_tagged`), one collection point (`Reconciler::settle_action`) and one render path (`reconciler::render_caveats`, opened through `Printer::section_caveats` / `output::AccentHeading`). **Never grow a second drain or a second render path.** A context nobody drains settles on the printer, so a standalone caller loses nothing.

`SystemContext`'s fields are private: `report` and `run_silent` are the whole surface, so `cx.printer.status_simple` is not expressible. **Never add a `printer()` accessor.**

## Source-constraint mode (every `compose_with_sources` call site)

**`ConstraintMode::Report` is for read paths; every path that mutates the machine composes in `Enforce`.** Decide on what the command *does*, not on what it reads: `backup run` reads config like `status` but executes hooks and writes snapshots, so it is `Enforce`. `Report` records a violation and continues; `Enforce` aborts on the first one.

| Mode | Commands |
|---|---|
| `Report` | `status`, `diff`, `verify`, `compliance *`, `backup list`, `checkin`, `decide` — anything whose whole job is to describe state (`decide`'s composition is a classification READ; its write is a decision-store row, and `Enforce` would disable answering exactly when a source violates a constraint) |
| `Enforce` | `apply`, `plan`, `daemon`, `backup run`, `backup restore`, `source add` — anything that runs a script, writes a file, or takes a snapshot |

`Report` is not "skip the check": `compose` still warns per violation, and any script surface a read path would EXECUTE is marked unrunnable in the composed spec (`composition::block_barred_scripts`). A new script surface a `Report`-mode command evaluates extends that marking too.

## Structured-output coverage

Every `cmd_*` function in `crates/cfgd/src/cli/` must have a row in `.claude/rules/structured-output-coverage.md`; `.claude/scripts/audit.sh` fails when one is missing.

## No `tracing::info!`/`warn!`/`error!` in the config/module/source domains

Banned anywhere under `crates/cfgd-core/src/{config,modules,sources}/` — the three domains turning user-authored YAML/TOML into typed config. Those macros write to a channel invisible without `RUST_LOG`, so an advisory routed there is one the user never sees. Use instead: collect the message into a `Vec<String>` the caller drains through `printer.deprecation(text)` — or `printer.alert(text)` for a run-affecting notice — at the command boundary; `CfgdConfig.deprecations` is the working example to extend.

The gate enforces this on the DOMAIN — every non-test `.rs` under those directories — never on a function-name shape. Escape hatch for a genuinely internal diagnostic, mirroring `native-ok:`:

```rust
tracing::warn!("cache miss for {}", key); // tracing-ok: internal cache-timing diagnostic, not user-facing
```

The marker counts only inside a comment, only with a reason, inherited only from the line or the line above. **Disqualified from the hatch regardless**: any message describing the user's own config, a key they wrote, or anything that changes what they should do next. "Internal" means the entire audience is someone already reading `RUST_LOG`.

## A tracing event never restates a Printer line

**If a `Printer` already says it on this path, the tracing event may not say it again** — the duplicate lands on the ONE stream the live region repaints, and a write bypassing the region strands the last paint of whatever bar is on screen.

```rust
// WRONG — the caller already prints "Signed artifact"
tracing::info!(reference = artifact_ref, "artifact signed with cosign");

// RIGHT — the fields are a debugging detail, the sentence is the Printer's
tracing::debug!(reference = artifact_ref, "artifact signed with cosign");
```

Demote to `debug!` when the event carries a field the printed line does not; delete it when it carries nothing extra.

**What the audit gate enforces, exactly.** `tracing::info!` is rejected in every non-test `.rs` under both crates, with one exemption and one hatch: **`daemon/` is exempt at any depth** (there the log IS the output, which is why the reconcile loop keeps `info` as its floor), and the `// tracing-ok: <why>` hatch applies. `warn!`/`error!` are NOT part of the audit gate — outside the three banned domains they reach the user. On the APPLY PATH (`reconciler/`, `packages/`) the two louder levels are mechanically enforced instead by `no_apply_path_warn_restates_a_printer_line` (`crates/cfgd/src/cli/tests.rs`): hatch or demote, because a restatement there lands at column 0 in the middle of the phase tree.

## The two mechanisms that keep tracing off the live region

One is the writer, in `output/`; the other is the default filter, in `main.rs`. Nothing outside `output/` may build a `MakeWriter` of its own, and nothing but `main.rs` picks a default filter.

- **`output::LiveTracingWriter`** (`output/tracing_writer.rs`) — the `MakeWriter` the binary installs: every event writes through the printer's `MultiProgress`, so it clears the bars, lands, and lets them repaint. Unattached, it writes plain stderr. The `every_subscriber_writes_through_a_folding_writer` fence (`output/tests/fences.rs`) keeps it the writer every subscriber takes, with a POSITIVE predicate; a non-terminal writer takes `// unfolded-writer-ok: <why>`.
- **`crates/cfgd/src/main.rs::tracing_filter_for(quiet, verbose, daemon)`** — the default filter: `warn` by default, `-v` debug, `-vv` trace, `--quiet` error. The RECONCILE LOOP keeps `info` as its floor (selected by `runs_reconcile_loop`); `daemon install`/`uninstall`/`status` report through the `Printer` and keep `warn`. `RUST_LOG` outranks all of it.

## `LiveBarState` is shared by every renderer writing one live region

`renderer::LiveBarState` (the live-bar count plus the broken-terminal latch) is held in an `Arc` and carried through `Printer::build_derived`: a derived renderer counting its own bars answers the routing gate "no bar is live" while the parent's spinner paints, and raw-writes over it. **Never mint a fresh `LiveBarState` for a renderer sharing an existing region.** A claim about a stranded paint is provable only on `Printer::for_test_live_terminal` (`shared-utils.md`, Test guards).

## The cursor is hidden for the life of the live region

`renderer::LiveBarGuard` — the ONE seam every bar passes through — hides the cursor as the count goes 0→1 and shows it as the count returns to 0 (`output/cursor.rs`); a per-bar toggle would flash it back mid-region. The first hide arms a `signal-hook` action on SIGINT/SIGTERM that restores the cursor, then emulates the default handler unless a cooperative owner called `output::claim_termination_signals()` first. A capture sink records the escape as a `TermLike` move; the scrollback capture carries neither escape, which is what keeps every golden a golden. `the_cursor_hides_on_the_first_bar_and_shows_when_the_last_drops` (`output/renderer/mod.rs`) pins it.
