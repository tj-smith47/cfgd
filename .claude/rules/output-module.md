---
paths: ["crates/**/*.rs"]
---
# cfgd Output System — critical design constraint

The `output` module (`crates/cfgd-core/src/output/`) provides the `Printer` struct: the sole interface for writing to the terminal. This file is the CATALOG of that surface and the RULES for reaching it; the reasoning behind each composer lives in its own rustdoc.

## Printer surface

Reach for the composer matching the call site's shape; never hand-build a `format!` string a composer owns.

- `heading(text)` — top-level title.
- `heading_title(&TitleLabel)` — a `Label: value` title, styled through `TitleLabel`'s three slots. `Doc::heading_title(label, value)` is the buffered entry point; `Doc::heading_title_typed(label, value, type_span)` lifts a span inside the value into `theme.type_hint`. A heading with no value part stays a plain `heading`.
- `heading_owner_prefixed(prefix, &OwnerLabel)` — a `<Verb> <owner>` heading (`Add source:acme`), and the ONLY heading slot an owner token may occupy. `Doc::heading_owner_prefixed(prefix, owner)` is the buffered entry point, for a command whose whole render is a `Doc`. No unprefixed counterpart exists: an owner names WHOSE the rows below it are, which is a section's job.
- `kv(key, value)` / `kv_block(pairs)` — a key/value fact and a block of them.
- `SectionGuard::kv_rows(rows)` / `Doc::kv_rows(rows)` / `SectionBuilder::kv_rows(rows)` — `kv_block` over hand-built `KvPair`s, and how a row reaches the three renderer-owned slots: `annotated(key, value, note)` (a muted parenthesised note about the value), `role_valued(key, value, role)` (the VALUE tinted by what it says, through the same `role_glyph` mapping every role-styled surface reads) and `nested(key, value)` (indented to say it belongs to the row above). **A caller never paints or indents one itself** — the fold would eat the coat, and a hand-padded key misaligns the block. All three are `#[serde(skip)]`, display-only, and why `Printer::muted` no longer exists.
- `command_list(pairs)` — a "command — description" list, `kv_block`'s counterpart for a left column that NAMES a thing rather than carrying data. No `KEY_WIDTH_CAP`, the canonical `" — "` glue, and a long description hangs to the DESCRIPTION column. `CommandPair::typed(key, type_span, value)` paints a type span `theme.type_hint`. `Doc` / `SectionBuilder` / `SectionGuard` counterparts exist.
- `Doc::paragraph(text)` — a prose paragraph: no glyph, no key column, no verbatim contract, for what a documentation surface says ABOUT the heading above it. Empty input emits nothing; buffered-`Doc` only.
- `status_simple(role, subject)` — a concise status line. `Role::{Ok, Info, Warn, Fail, Skipped, Pending, Running, Accent, Secondary}`; `Accent` is "attention without alarm", `Secondary` a "structural pivot / label / identifier". Both are iconless and suppressed at `Verbosity::Quiet` like every non-`Fail` role.
- `status(role, subject)` — a `StatusBuilder` for `.detail`, `.duration`, `.label(role, text)`, `.qualifier(text)`, `.drift(expected, actual)`, `.with_data`. `.qualifier` composes `subject: qualifier` as THREE slots — a colon that is part of the DATA (a path, a URL) is not a qualifier. `.drift` is the ONE canonical `want: X, have: Y` spelling. `doc::StatusFields` carries both for the buffered path, via `status_with`.
- `hint(text)` / `note(text)` — supplementary output.
- `deprecation(text)` — the SPELLING the user reached for is on the way out. Always visible, stderr-only so the `-o` channel stays pure; keeps the top-level structural assert, being drained at the command boundary.
- `alert(text)` — a persistent advisory about what THIS run will actually do. Separate from `deprecation` because a deprecation is about spelling and an alert about effect, and not a substitute for `status_simple(Role::Warn, …)`, correctly suppressed under `-o json`. The ONE always-visible emit correct at any depth.
- `table(table)` — tabular data.
- `section(name)` — a `SectionGuard` (drop ends it) carrying `bullet`/`kv`/`kv_block`/`command_list`/`hint`/`note`/`table`/`code_block` plus `.diff` and `.syntax_highlight`. A nested plain section paints its heading `theme.secondary`, so nesting reads from styling alone; a first-level or OWNER section is unaffected.
- `section_title(&TitleLabel)` — a `Label: value` section head (`Restore: notes`), `heading_title`'s sectioned counterpart and the only titled heading that can carry a block of rows. A heading plus a top-level `kv_block` puts the rows at the heading's own indent, where they read as the command's output rather than as facts about the run above them.
- `section_owner(&OwnerLabel)` / `section_owner_or_collapse(&OwnerLabel)` — a top-level section headed by a styled owner token; `_or_collapse` leaves no trace when nothing renders inside it. `SectionGuard` carries the nested pair, `Doc::section_owner` / `subsection_owner` the buffered ones. A `Doc` belonging wholly to one owner opens with `Doc::section_owner` — the owner is never the `Doc`'s heading.
- `diff(old, new)` / `syntax_highlight(code, lang)` — nest at whatever depth is ambient, flushing any pending section header first like every other emission.
- `spinner(label)` — a `Spinner` with `.finish_ok` / `.finish_fail(...).detail(e)` / `.set_message`. Abandoned without an explicit finish, `Drop` settles it `Role::Skipped` + `" (interrupted)"`, the one honest record. A spinner borrowed from a `LiveRow` is exempt: the row settles its own line.
- `narrate(running, |sp| …)` — the settle-safe wrapper, for a long wait whose failure NOBODY ELSE reports. Success retires the bar SILENTLY, so a successful run's permanent output is byte-identical and every golden stays a golden; failure settles `Role::Fail` at whatever step `set_message` last named, with no detail. **Reach for this instead of a hand-rolled spinner + match at any site whose body can `?`.** A wait inside a command deriving a Quiet printer is narrated through the OWNING printer.
- `narrate_silent(running, |sp| …)` — the same for a wait whose OUTCOME LINE belongs to somebody else. The criterion is who else SAYS the failure, never how the `Err` travels — propagating to the CLI boundary is NOT what makes a wait a `narrate` site. `Role::Fail` survives `Verbosity::Quiet`, so every duplicate this prevents would land beside a `-o json` payload carrying the same fact.
- `Spinner::finish_silent()` — retire a bar printing nothing. Reach for it directly only where neither wrapper fits: an `output/` internal, or a site with no printer to call a wrapper on.
- `progress_bar(...)` — `.inc`, `.set_position`, `.set_message`, `.finish(self)`, and `Drop` parity with `Spinner`. A loop driving one bar across many items routes its fallible work through an inner fn and calls `.finish()` on EVERY exit path.
- `live_row_at(depth)` / `live_row_after(depth, &row)` / `live_row_first(depth)` — a `LiveRow`, ONE line of the live region the CALLER owns and rewrites in place (`set_action_status`, `window`, `set_owner_label`, `set_note`, `retire`). `retire` ERASES rather than commits; the permanent line goes separately into a `SectionGuard`. `live_row_budget()` reports how many ROWS fit before truncation from the FOOT (a row count, not the terminal LINES a soft-wrapped or child-tailing row can paint — `LIVE_REGION_HEADROOM` covers that slack) — spend it on rows with nothing left to say, and NEVER retire a running row.
- `run(cmd, fmt)` — buffered command execution with live output.
- `data_line(text)` — a raw structured-output line.
- `emit(doc)` — `Doc` emit (for `-o json|yaml|jsonpath|template`).

## One titled section per command — a result section never respells the title

A command's output is headed ONCE, by the thing the command is. `cfgd module push` opened a `Push Module` heading, printed its header facts under it, then opened a `Push` section for the result rows — two headings, one of them a word already spent, and a nesting level that carried no information because everything the command did lived inside it.

```
BAD                                   GOOD
────────────────────────────────      ────────────────────────────────
Push Module                           Push Module
  Directory: ./mod                      Directory: ./mod
  Artifact:  ghcr.io/a/b:v1             Artifact:  ghcr.io/a/b:v1
  Push                                  ✓ Pushed module
    ✓ Pushed module                     Digest: sha256:…
    Digest: sha256:…                    ✓ Signed with cosign
```

The rule: **the command's title IS its section**, opened with `printer.section("<Verb> <Noun>")` plus `let _inherit = printer.depth_inheritance();`, and the header facts, the result rows and every owner sub-section go inside it. A second section under that title may not respell any word the title spent — `Push Module` → `Push` is banned, `Sync` → `Sources` is fine, because a sub-section names a DIFFERENT subject, never the same one again.

`no_result_section_respells_a_word_its_command_title_already_spent` (`crates/cfgd/src/cli/tests.rs`) walks every production `.rs` under `crates/cfgd/src/cli/`, pairs each file's `printer.heading` / `printer.section` literals, and fails on the overlap.

## Wording rules every closing line and hint obeys

Eight conventions, each with a walk-the-population pin that fails on the next member that breaks it. All eight live in `crates/cfgd/src/cli/tests.rs` unless noted.

| Rule | Shape | Pin |
|---|---|---|
| A result line is **sentence case, past-tense verb first, count after** | `✓ Accepted 1 item`, `✓ Subscribed`, `✓ Installed daemon service` — never `Daemon service installed`, never Title Case. The counterpart grammar is a run BODY row's (lowercase imperative, below); the two pins **partition** the sources — a subject slot under `reconciler/` or `backup/` is a body row, everything else in both crates is a result line | `every_result_line_is_sentence_case` |
| A message naming a command **quotes it in backticks** | ``Run `cfgd decide accept <resource>` to answer`` — never `'cfgd …'`; covers hints, errors and clap `///` help, in both crates | `every_command_a_message_names_is_quoted_in_backticks` |
| The **up-to-date verdict is not a command's to word** | every no-actions verdict settles through `reconciler::nothing_to_do_verdict(pending)`, which answers `Role::Pending` + `Nothing to apply — N decisions pending` whenever something is withheld | `no_command_words_the_up_to_date_verdict_for_itself`, `a_pending_decision_denies_the_up_to_date_verdict_on_every_verdict_surface` |
| A **rendered label is Title Case**, whichever slot holds it | a kv key, a `KvPair`, a row tuple and a table header all read `Last Sync` / `Drift Count` — never `Reconcile interval` two rows above a Title Case column. Small words stay lowercase off the front (`Signing with`); a label NAMING a thing (a `spec.packages` path, a tool's own name) keeps that spelling under a `// name-row-ok:` marker | `every_rendered_label_is_title_case` |
| A **`source` verdict is counted iff the verb takes many subjects**, and every mutating `source` or `module` verb **closes its success path on a next step** | `✓ Updated 1 source` (the one verb that can run over every source) beside bare `✓ Subscribed` / `✓ Removed`; each followed by `success_next_step`'s hint — `cfgd sync` for a trust edit, plan/apply for a composition edit, the consumer for an artifact (`push` → a Module resource or `--apply`, `build` → `module push`, `pull` → `module show`). A hint never re-spells the verb that just ran with concrete arguments, and never names a file that verb CONSUMES — `push`'s `<module>.yaml` substituted to the `module.yaml` the verb requires, so applying it undid the push. A verb that genuinely ends a workflow is hatched in the walk's table with its reason | `every_source_verdict_counts_iff_its_verb_takes_many_subjects`, `every_mutating_verb_closes_on_a_next_step`, `every_composed_next_step_names_a_command` |
| A **count belongs to its section's annotation**, not to a row | `Pending Decisions (1 item)` via `reconciler::pending_decisions_title`, never a `⊙ 1 pending item` line that wears the row glyph and the row indent | `pending_decisions_title`'s own unit tests + the `decide` / `status` goldens |
| A **section's instruction closes it from inside** | the answer hint under a Pending / Declined Decisions section is the SECTION's, rendered at its depth straight under its last row (`section.hint(…)` in `ApplyRun::render_withheld`, `s.hint(…)` in `source::helpers::build_pending_decisions_table_section`) — never a `Doc::hint` hung off the document a blank line lower and an indent shallower on one surface and not another. Those two composers are the only emitters of `answer_decisions_hint` / `MSG_INCLUDE_DECLINED_DECISIONS`; a surface listing decisions renders through one of them | `every_decisions_hint_closes_its_section_from_inside` |
| The **closing line holds one em-dash and one trailing parenthetical** | `✓ Apply complete — 21 actions succeeded, 1 not attempted: no session manager (278.2s wall)` — the em-dash joins the title to its detail, the count list is comma-joined with the withheld clause last and its reason after a colon, and the one `(…)` is the elapsed. `(1 not attempted — no session manager) (278.2s)` nested a second em-dash under the first and set a caveat's parenthetical beside a measurement's, on the line a reader stops on. `outcome_counts` is the ONE composer; `ApplySummary`'s prose and the daemon's `reconcile: complete — …` log line read the same counts | `the_closing_line_holds_one_em_dash_and_one_trailing_parenthetical` (`reconciler/run/tests.rs`) |
| A **closing hint names the command that comes next**, in backticks | ``Run `cfgd apply` to …`` after `decide accept`, ``Run `kubectl rollout status …` `` after `inject`, ``Change it with `cfgd source priority <name> <n>` `` — never `Changes will take effect on next reconcile`, which names a background loop a daemon-less machine never runs while the reader's next keystroke is the command the tool declined to name. A hint whose text is built elsewhere is pinned by its producer; the walk covers `crates/cfgd/src/cli/`; a genuinely command-less instruction there carries `// hint-ok: <why>`. A RUN's verdict closes the same way: every non-`Success` rollup takes `reconciler::run_next_step`, and a verdict line states facts only — `rerun to converge` baked into `Aborted`'s detail was an instruction no sweep over the hint composers could find | `every_unfinished_verdict_closes_on_the_one_next_step` (`reconciler/run/tests.rs`), `every_closing_hint_names_a_command` |

## Rendering rules every action row obeys

Five conventions about the SHAPE of a settled row, each with a walk-the-population pin (in `crates/cfgd-core/src/` unless noted). A row is painted by one of three surfaces (`Printer::action_status`, `SectionGuard::action_status`, `LiveRow::set_action_status`) and settled by two trees (`render_plan_tree`, `Reconciler::settle_action` → `emit_action_line`), so every rule here is enforced at the ONE seam all five read, never at a call site.

| Rule | Shape | Pin |
|---|---|---|
| A **withheld row holds its subject back and lets the reason speak** | `Pending` / `Skipped` render the subject muted and the detail bright; every other role does the reverse. Both halves answer from `renderer::{action_subject_style, action_detail_is_muted}`, so the plan tree and the apply tree paint the same bytes for the same row | `every_role_takes_one_emphasis_on_both_halves_of_an_action_row`, `the_withholding_split_has_a_member_on_each_side` (`output/renderer/glyphs.rs`), `both_trees_paint_a_withheld_row_with_the_same_bytes` (`reconciler/run/tests.rs`) |
| **One alignment column per report**, not per phase | the elapsed/detail column is measured once over every action the run will print and claimed through `Printer::report_column`, so it never moves x position between phases — including the pseudo-phases (`Backups`, `cfgd:env`) a `Plan` does not carry | `report_align_width_spans_every_phase`, `report_align_width_ignores_a_filtered_out_phase` (`reconciler/run/tests.rs`) |
| A **pre-skipped action is priced by ONE predicate at both ends** | `Action::pre_skip_reason` keeps it out of `Actions N planned` AND out of the counted rollup: its row keeps the reason, the verdict names it as the last clause of its count list — `21 actions succeeded, 1 not attempted: no session manager` — never as a `skipped` that ran | `a_pre_skipped_action_is_priced_outside_the_counted_rollup` (`reconciler/run/tests.rs`) |
| A **wrapped row lands its duration at the group's column, never at the terminal edge** | the column `group_column` settled for the set (the one every sibling pads its subject to) is threaded through `emit_status_line` → `push_line_undrained` → `wrap::wrap_body_with_trailer`; a wrapped row whose last line ends short of it pads out to it, one already past it glues the trailer inline, and a group with NO column (`group_column` answered 0) glues on every row alike. Right-aligning to `cols` put the one wrapped row's `(23.8s)` twenty columns right of siblings that all glued theirs | `a_wrapped_row_in_a_group_with_no_column_glues_its_trailer_inline`, `a_wrapped_row_in_a_group_with_a_column_pads_its_last_line_to_that_column`, `a_wrapped_row_already_past_the_group_column_glues_its_trailer_inline` (`output/renderer/wrap.rs`); `a_wrapped_row_outside_any_aligned_group_glues_its_duration_inline`, `a_wrapped_row_in_a_live_group_lands_its_duration_at_the_groups_column` (`output/renderer/status.rs`) |
| A **run's total is wall-clock and says so; a row's span never does** | the closing rollup composes its elapsed through `StatusBuilder::wall_duration` → `Elapsed::wall`, rendering ` (278.2s wall)`; every action row, spinner finish and command window takes `.duration` → `Elapsed::row`, ` (23.8s)`. Package and provisioner lanes run concurrently, so a column of row spans legitimately sums to more than the total — the word is what tells a reader adding them up why | `a_wall_clock_total_says_wall_and_a_row_span_does_not` (`output/renderer/status.rs`), `the_closing_line_holds_one_em_dash_and_one_trailing_parenthetical` (`reconciler/run/tests.rs`); the snapshot normalizer keeps the word (`normalize_snapshot_durations_keeps_the_wall_clock_word`) |
| An **action row's subject opens on a lowercase verb** | `create ~/.zshrc`, `brew install jq`, `run preApply script`, `snapshot notes.md.<stamp>`, `restore ~/notes.md from notes.md.<stamp>` — the row names WORK and the glyph says how it went. The sentence-case, past-tense grammar is the closing line's (`✓ Restore complete`), never a body row's: `backup restore` wrote `Restored from …` into the slot `backup run` writes `snapshot …` into, and the sentence-case pin would have rewarded it. Two verbs of one command mint their subjects side by side (`backup::snapshot_subject` / `restore_subject`); a subject opening on a proper noun takes a `// name-row-ok:` marker | `every_action_row_subject_opens_on_a_lowercase_verb` (`crates/cfgd/src/cli/tests.rs`) — walks every subject slot in `reconciler/` and `backup/`, following `let` bindings and producer functions to the literal |
| A **duration slot never renders a zero** | anything the one-decimal form would round to `0.0` renders ` (<0.1s)` — a sub-tick action DID run, and `(0.0s)` says it took no time. One composer (`renderer::status::duration_text`) feeds the action suffix, the spinner finish and the rollup total alike | `a_sub_tick_duration_renders_the_floor_and_never_a_zero`, `every_duration_slot_composes_through_the_one_trailer` (`output/renderer/status.rs`) |

### The grammar split, stated once

Three grammars share one screen, and which one a string takes is decided by WHAT the string is, never by which surface prints it:

| Kind | Grammar | Example |
|---|---|---|
| **Body row of a run** — previewed by the plan and settled by the apply, ONE string in both slots | lowercase imperative | `create ~/.zshrc`, `provision npm via apt (rustc)` |
| **Result line** — an outcome reported once, never previewed | sentence case, past-tense verb first | `✓ Installed daemon service`, `✓ Cloned repository` |
| **Provider note** — a `.report(` body under a settled row | a sentence, or the command as it was run | `Updated /etc/environment`, `systemctl restart foo` |

Headings are not in any of the three: a heading is a Title Case label (`every_rendered_label_is_title_case`). A note echoing a command keeps the command's own spelling, which is why the note population is judged by `every_provider_note_takes_its_role_from_whether_the_reader_must_act` on its ROLE and not on its case.

Both snapshot normalizers know the floor's spelling (`util/paths.rs::duration_span`, `output/test_capture.rs::strip_spinner_duration`), so a golden stays host-stable across it.

## A run's two ends come from one skeleton

**A command that closes with the shared rollup opens with the shared header.** `cfgd backup restore` took `render_run_rollup`'s footer — the `✓ Restore complete — 1 action succeeded` verdict — while printing none of the `Config` / `Profile` / `Sources` rows `ApplyRun::header` exists for, so the one verb that overwrites live data was also the one that never said which config it acted under.

Both ends come from `reconciler::ApplyRun`. A run whose body the caller renders and whose work is neither a `Plan` action nor a `spec.backups[]` unit builds one through `ApplyRun::unplanned(ctx, actions)` — never a synthesized empty `Plan`, and never a second `Config`/`Profile`/`Sources` renderer. `RunContext::subject` is what puts the acted-on unit in the title (`Restore: notes`), and `RunContext::unit_source` puts that unit's declared path in the header as a `Source` row beside `Config`/`Profile` — an INPUT fact of the run, so it heads the run rather than trailing an action row as a `Destination` kv; the header renders BEFORE any confirmation prompt, because an operator agreeing to overwrite live data is agreeing under a config and a profile.

`every_run_that_renders_the_rollup_also_renders_the_run_header` (`crates/cfgd/src/cli/tests.rs`) walks both crates' production sources and fails on a function calling `render_run_rollup` / `render_apply_result` whose body renders no header. A helper whose caller already rendered one carries a `// run-header-ok: <why>` marker.

**A run's only phase prints no phase row.** `Phase: Backups` earns its line inside `cfgd apply`, beside `Packages` and `Files`; on `cfgd backup run` and the daemon's scheduled fire it restated the title one indent down (`Backup` / `Phase: Backups` / `backup:notes`) while `backup restore` put the same owner group one level shallower. A plan-less `ApplyRun` renders its backups through `reconciler::sole_phase` — owner groups at the run's own depth — and a run carrying a `Plan` keeps `pseudo_phase(BACKUPS_PHASE_LABEL)`, filtered phases included (there the label states the filter). `sole_phase_renders_its_owner_groups_at_the_run_depth` (`reconciler/run/tests.rs`), `a_backup_run_prints_no_phase_row_for_its_only_phase` (`backup/tests.rs`) and the raw-render half of `backup_group_is_identical_across_surfaces` (`daemon/tests.rs`) pin the split.

## Blank lines: the composer owns the separator

**One blank line between sibling blocks; none at the start, none at the end, none after a heading that owns rows, never two in a row.** Every blank line a surface shows is a producer's, never a call site's: a top-level section close and a top-level group boundary (`TopGroup` — a run of status, hint or bullet lines) each ARM one pending blank (`RenderState.blank_pending`), and the next line drains it. A heading binds its rows, so no blank follows it. A streamed child line (`render_stream_line`, git's own `Cloning into …`) is the body of the status line that announced it: it consumes the pending blank and re-arms the boundary, so the section after a passthrough gets its blank while a finish line (`✓ Cloned`) still binds to what it announced. `data_line` writes a payload as ONE line, so a serialized document ending in `\n` closes on one newline, never a blank.

```
BAD (a stream line cleared the boundary)     GOOD
──────────────────────────────────────       ──────────────────────────────────────
◐ Cloning repository                         ◐ Cloning repository
    Cloning into '/x'...                         Cloning into '/x'...
    done.                                        done.
Plan
  Config  /x/cfgd.yaml                       Plan
                                               Config  /x/cfgd.yaml
```

A title that owns no rows of its own (`Doctor`, `Verify`, `Checkin`) is a DOCUMENT title, and the blank under it separates it from its first section; a titled RUN (`Diff: nvim`, like `Apply` / `Plan` / `Status: nvim`) carries its header rows, so `cfgd diff --module` prints `Config` / `Modules` under its title the way `apply --module` does. A call site never emits a bare blank line (`printer.blank()` / `println!()`); a missing blank names a composer that cleared the boundary without re-arming it.

Pins: `every_golden_separates_sibling_blocks_with_one_blank_line` (`crates/cfgd/src/cli/tests.rs`) walks every `.txt` golden under `crates/cfgd/tests/output_snapshots/**` and every `snapshots/` directory under `cfgd-core/src` and fails on a leading, trailing or doubled blank, on a blank straight under a heading that owns rows, and on two sibling blocks touching; `a_block_after_streamed_output_gets_its_one_blank_line` (`output/renderer/mod.rs`) pins the stream boundary and `data_line_closes_a_newline_terminated_payload_on_one_newline` (`output/raw.rs`) the data path.

## Rules every key/value block and shared table obeys

| Rule | Shape | Pin |
|---|---|---|
| A **command's produced facts are its rows' details, never rows between them** | the INPUT facts of a command are ONE kv block before its action rows (`Directory` / `Artifact` / `Platform` under `Push Module`, `Old Commit` / `New Commit` under `Update module:x`, `Commit` / `Integrity` opening a module review); a fact a step PRODUCES is that step's row detail (`✓ Pushed module — sha256:69c3…`, `✓ Enrolled as user 'alice' — team platform, device dev-1`, `✓ Checked in — server status ok, config unchanged`) or a closing kv block after the LAST row (`upgrade`'s `Installed to`, `pull`'s `Module` / `Packages` / `Files`). A kv row never sits between two result lines: `Digest` between `✓ Pushed module` and `✓ Signed artifact` made one command read as verdict, fact, verdict. Two verdicts about one thing (`✓ Daemon running`, `⚠ Update available`) sit together above the facts block. A kv row a branch prints INSTEAD of a status (`generate --scan-only`'s empty-state row) carries `// facts-block-ok: <why>` | `no_kv_row_sits_between_two_result_lines` (`crates/cfgd/src/cli/tests.rs`) walks every `cli/**` function body lexically (a status, then a kv, then another status with no new section, heading or `Doc` between); narration a callee prints (`oci::push_module`'s spinner, `sign_and_attest`'s verdict) is out of its sight, so the `module_push/pushed`, `module_build`, `image_pack/*`, `enroll/cmd_token_flow` and `checkin/*` goldens pin those shapes. The action-row half — a count a step produces is the row's DETAIL (`deploy a, b — 6 files`), never a parenthetical on its subject (`deploy a, b (6 files)`) — is produced once by `reconciler::action_produced_detail` for the plan bullet, the apply row and the `-o json` plan `detail`, and `every_produced_count_is_an_action_rows_detail` walks `reconciler/format.rs`'s subject literals for a `({} <noun>)` trailer |
| **One key column per section**, whatever else the section printed between the blocks | a section opening `Directory` / `Artifact`, printing its action rows, then closing on a kv block pads the second block to the column the reader is already scanning. The width is carried on the open `SectionFrame` (`kv_key_col`) rather than computed at close, for the same reason `live_column` is: the rows in between are already on the terminal, and a live section cannot wait to learn its own width | `a_section_keeps_one_key_column_across_everything_it_prints` (`output/renderer/kv.rs`) |
| A **kv value never hand-builds the annotation slot** | `KvPair::annotated(key, value, note)` owns the muted `value (note)` — never `KvPair::new("Source", "remote (locked)")`, which paints the note in the value's colour and hides it from anything reading the value | `no_kv_value_hand_builds_the_annotation_slot` (`crates/cfgd/src/cli/tests.rs`); table cells and status details are out of class and named in its doc |
| A table **drops a column no row can fill** | every `Table` the CLI renders settles through `Table::without_unfillable_columns` before it is emitted, so `source list` loses `Drift` (only the daemon holds it) and `backup list` loses `Schedule` / `Status` / `Next Run` before the first run, rather than padding a column of `-`. The absence token is the ONE `cfgd_core::ABSENT` (`yes_no(None)`, `humanize_until_cell(None)`, every hand-filled cell), which is what lets one predicate judge a column; `-o json` keeps every field | `every_listing_the_cli_renders_drops_a_column_no_row_can_fill` (`crates/cfgd/src/cli/tests.rs`) walks every `Table::new` in the CLI; `a_column_every_row_leaves_absent_is_dropped_with_its_roles` (`output/renderer/table.rs`) and `a_column_no_row_can_fill_is_dropped_from_the_listing` (`cli/source/list.rs`) pin the behaviour |
| Every surface listing config sources renders **the one `Sources` table** | `source list`, `status` and `daemon status` all build through `source::list::sources_table` under `SOURCES_SECTION`, so no two of them name the same source with different columns. A surface holding live facts merges them OVER a `configured_source_entries` row instead of building a narrower table | `both_sources_surfaces_render_through_the_one_table_builder` (`crates/cfgd/src/cli/tests.rs`) |

## The reconcile loop has no printer to report to

A daemon under systemd/launchd is read through its journal, so the loop's own account of itself — startup, each tick's outcome, shutdown — goes to `tracing` and NOTHING to the `Printer`. A printed duplicate is a second copy of the same sentence on the stream the live region repaints.

`daemon/service/` is the exception at any depth: install and uninstall are one-shot commands run by somebody watching a terminal, so those DO report through the printer. `every_daemon_info_event_names_its_subsystem` (`output/tests/fences.rs`) walks the loop's own files and fails on a `heading` / `status_simple` / `status` / `status_with` call outside `service/`.

## Sanitizing text cfgd did not author

`cursor_safe` (`output/mod.rs`) is the ONE renderer FOLD, and it covers every slot above that carries caller text. **A call site echoing a gateway field, a remote source's description or a tool's captured stderr through one of those slots does NOT sanitize it by hand.**

Two other policies exist and are not interchangeable with it: a PRE-APPROVAL surface ESCAPES (the fold strips ANSI, and a screen the operator approves from has to SHOW it), and `prompt_text`'s `default` STRIPS (a default is returned AS the answer). Every payload — a `plain()` form, a persisted string, `-o json`, `data_line` — stays byte-exact.

**The full routed-slot inventory, the escape/strip surfaces and the five terminal writers that take no policy are in `output/mod.rs`'s module doc.** A new slot rendering caller text routes through `cursor_safe` and is added there.

**An error `Doc` under a selector format always echoes to stderr first.** `emit_structured` (`output/structured.rs`) routes `-o name` / `jsonpath=` / `template[-file]=` through the reader's SUCCESS-shaped selector, which an error doc's shape almost never satisfies — so a non-matching selector printed nothing anywhere, leaving only the exit code. `doc.error_message()` now reaches `sink_stderr` unconditionally before the selector runs, and the selector may separately match a field, so a selector format can render an error twice. Kept in sync with `docs/cli-reference.md`'s "Error output".

**Every module receives a `&Printer` (or `Arc<Printer>` in async contexts). This is non-negotiable.**

## Collapsing an error or a script body into a subject

**Collapse a captured error before it becomes a status subject.** Route an `io::Error`, `CfgdError` or command stderr through `cfgd_core::output::collapse_to_subject_line(err)`: an error's line breaks are an artifact of capture, not structure, and a one-line subject is what scans.

A subject that is *genuinely* multi-line — a brew caveat is two sentences — may carry `\n`; the renderer lays those out as continuations indented to the marker column. Never hand-roll that indent: a `\n` continuation must look identical to a soft-wrapped one.

**A DETAIL takes `cfgd_core::output::captured_output_detail(err)` instead.** Same bound, no flattening: the row's ` — ` is the subject/detail separator, so spending it again between a child's own lines renders an error chain as `Caused by: — failed to download …`. Both folds cap at the live window's own `VISIBLE_LINES` and elide from the middle, keeping the head that names the failure and the tail that diagnoses it — `cargo install` writes forty `Downloaded <crate>` lines to stderr before its one real sentence, and an uncapped fold put twenty-one physical lines in one action row and pushed the run's own header off the screen. Display-only: the journal, `ActionResult.error` and `-o json` keep the full text.

For a user-authored script body landing in a subject, route through `cfgd_core::output::condense_script_label(body)` instead — a lossy, DISPLAY-only summary. Never use it for:

- **persisted / machine-matched strings** (a resource-id, journal `resource_id`, `ActionResult.description`, a `-o json` field) — they stay byte-identical to the raw body.
- **pre-approval security-review contexts** (a module `add`/`upgrade` diff, `print_module_review_summary`) — the user must see the FULL script before approving it. Use `bullet()` or `code_block()`.
- **"not found" echoes of a user-typed search argument** — prefer `collapse_to_subject_line`, since hiding the tail of the string that failed to match defeats the error.

Holding an `Action` and its already-formatted description, call `cfgd_core::reconciler::condense_action_desc_for_display(action, desc)` rather than deciding per call site.

## Forbidden outside the `output/` module

- `println!`, `eprintln!`, `print!`, `eprint!`
- `console::*` direct use
- `indicatif::ProgressBar::new` / `MultiProgress::new` directly
- `log::*` macros — use `tracing::*`
- These method names are reserved-banned (rejected by `.claude/scripts/audit.sh` outside `output/`): `success`, `warning`, `info`, `error`, `header`, `subheader`, `key_value`, `newline`, `plan_phase`, `stdout_line`.

See Hard Rule #1 in `hard-rules.md`.

## Provider narration goes to the note sink, never to the printer

A `PackageManager` or `SystemConfigurator` executes UNDER an action line the reconciler settles from the plan, so a `status_simple` from inside one lands *above* the line describing the same work, outside the phase tree. Both traits carry a context whose `report` is the narration channel:

```rust
// PackageManager — the tag names the speaker, because the action line names the package
cx.report(Role::Warn, self.name(), "brew: run `brew link --force`");

// SystemConfigurator — no tag: the action line already reads system:<name>.<key>
cx.report(Role::Info, format!("systemctl {action} {name}"));
```

Every note collects into the run's `ApplyResult.caveats`, grouped by the `kind:name` owner that produced it, and renders once as a closing `Caveats` section:

```
✓ set sysctl.net.ipv4.ip_forward: 0 → 1     ← the reconciler's line, from the plan

✓ Apply complete — 1 action succeeded (0.1s)

Caveats
  profile:work
    ⚠ reload deferred: /proc is read-only     ← Warn renders before Info within a group
    ◉ sysctl -w net.ipv4.ip_forward=1
```

**A note's role answers ONE question — must the reader act?** A caveat or a degraded fallback (npm installing into a fallback prefix because the configured one is unwritable, a batch retried per package, a restart that failed) is `Role::Warn`; a report of work done on the side (`Updated /etc/environment`, `Created service X`, `Bash completion has been installed to: …`) is `Role::Info`; an instruction (`Add `. ~/.config/cfgd/env.sh` to your shell rc`) goes through `SystemContext::next_step` / `NoteSink::next_step` and renders as a `→` hint; nothing is `Role::Ok`, a second `✓` under a row the reconciler already settled. Brew's `==> Caveats` is a SECTION, not a severity, so a brew body is classified by `packages::shared::brew_caveat_asks_the_reader_to_act`; a line a tool itself labelled `warning:` keeps it. `every_provider_note_takes_its_role_from_whether_the_reader_must_act` (`crates/cfgd/src/cli/tests.rs`) walks every `.report(` literal under `packages/**` and `system/**`; `a_brew_caveat_is_a_warning_only_when_it_asks_the_reader_to_act` (`packages/shared/tests.rs`) pins the classifier.

Both land in one `NoteSink` and route through one rule (`NoteSink::report_tagged`), one collection point (`Reconciler::settle_action`, called from both dispatch paths) and one render path (`reconciler::render_caveats`, opened through `Printer::section_caveats`, whose heading composes through `output::AccentHeading` rather than a hand-styled string). `cli::plan_ops::print_caveats` is the one assembler for a real `cfgd apply`; a snapshot bridge is the only other caller. **Never grow a second drain or a second render path.** A context nobody drains settles on the printer, so a standalone caller loses nothing.

`SystemContext`'s fields are private: `report` and `run_silent` are the whole surface, so `cx.printer.status_simple` is not expressible rather than merely discouraged. **Never add a `printer()` accessor.** A snapshot bridge driving a configurator directly renders through `render_caveats` after its own closing summary, so its golden pins the real run-wide shape.

## Source-constraint mode (every `compose_with_sources` call site)

**`ConstraintMode::Report` is for read paths; every path that mutates the machine composes in `Enforce`.** Decide on what the command *does*, not on what it reads: `backup run` reads config like `status` but executes hooks and writes snapshots, so it is `Enforce`. `Report` records a violation and continues; `Enforce` aborts on the first one.

| Mode | Commands |
|---|---|
| `Report` | `status`, `diff`, `verify`, `compliance *`, `backup list`, `checkin`, `decide` — anything whose whole job is to describe state (`decide`'s composition is a classification READ; its write is a decision-store row, and `Enforce` would disable answering exactly when a source violates a constraint) |
| `Enforce` | `apply`, `plan`, `daemon`, `backup run`, `backup restore`, `source add` — anything that runs a script, writes a file, or takes a snapshot |

`Report` is not "skip the check": `compose` still warns per violation, and any script surface a read path would EXECUTE is marked unrunnable in the composed spec (`composition::block_barred_scripts` poisons a barred source's `patch.script`, so evaluating the file degrades instead of running it). A new script surface a `Report`-mode command evaluates extends that marking too.

## Structured-output coverage

Every `cmd_*` function in `crates/cfgd/src/cli/` must have a row in `.claude/rules/structured-output-coverage.md`; `.claude/scripts/audit.sh` fails when one is missing.

## No `tracing::info!`/`warn!`/`error!` in the config/module/source domains

Banned anywhere under `crates/cfgd-core/src/{config,modules,sources}/` — the three domains turning user-authored YAML/TOML into typed config. Those macros write to a channel invisible without `RUST_LOG` (`info!` least of all, the binary's default filter being `warn`), so a legacy-key deprecation routed there is an advisory the user never sees.

Use instead: collect the message into a `Vec<String>` the caller drains through `printer.deprecation(text)` — or `printer.alert(text)` for a run-affecting notice — at the command boundary that owns a terminal. `CfgdConfig.deprecations` is the working example to extend, not to reinvent per call site.

The gate enforces this on the DOMAIN — every non-test `.rs` under those directories — never on a function-name shape: an earlier revision anchored on `fn parse_*` / `fn load_*` and missed `warn_on_legacy_theme_keys` and every advisory helper a parse function calls.

Escape hatch for a genuinely internal diagnostic, mirroring `native-ok:` / `spawn-blocking-ok:` — mark the call line or the comment line directly above it:

```rust
tracing::warn!("cache miss for {}", key); // tracing-ok: internal cache-timing diagnostic, not user-facing
```

The marker counts only inside a comment, only with a reason after it, and is inherited only from a comment line — a call cannot exempt itself by naming the hatch in its own message string, and a marked call does not exempt the unmarked call beneath it.

**What disqualifies a message from the hatch**, whatever the marker says: a message describing the user's own config, a key they wrote, a migration they must perform, or anything that changes what they should do next. "Internal" means a diagnostic whose entire audience is someone already reading `RUST_LOG`.

## A tracing event never restates a Printer line

**If a `Printer` already says it on this path, the tracing event may not say it again.** The duplicate is a second copy of the same sentence on the ONE stream the live region repaints, and any write there bypassing the region strands the last paint of whatever bar is on screen. `cfgd module push` printed its result three times that way and froze its spinner doing it.

```rust
// WRONG — the caller already prints "Signed artifact"
tracing::info!(reference = artifact_ref, "artifact signed with cosign");

// RIGHT — the fields are a debugging detail, the sentence is the Printer's
tracing::debug!(reference = artifact_ref, "artifact signed with cosign");
```

Demote to `debug!` when the event carries a field the printed line does not; delete it when it carries nothing extra. A non-duplicate stays at its own level, carrying the `// tracing-ok: <why>` marker inside the banned domains.

**What the audit gate enforces, exactly.** `tracing::info!` is rejected in every non-test `.rs` under `crates/cfgd-core/src` and `crates/cfgd/src` — the whole of both crates — with one exemption and one hatch:

- **`daemon/` is exempt at any depth in either crate.** There the log IS the output: a service under systemd/launchd prints its ticks to journald through this channel and no other, which is why `cfgd daemon run` keeps `info` as its tracing floor.
- **The `// tracing-ok: <why>` hatch applies**, read exactly as the domain gate's is.

`warn!` and `error!` are NOT part of the AUDIT gate — outside the three domains above they reach the user, the default filter being `warn`. `info!` outside the daemon is a line nobody reads AND a strand risk when they do.

On the APPLY PATH the two louder levels are mechanically enforced instead, by `no_apply_path_warn_restates_a_printer_line` (`crates/cfgd/src/cli/tests.rs`): every `tracing::warn!` / `tracing::error!` under `crates/cfgd-core/src/reconciler/` and `crates/cfgd/src/packages/` either carries the same `// tracing-ok: <why>` hatch or is demoted to `debug!`. That population is where a restatement costs the most: at the default filter the event lands at **column 0** in the middle of the phase tree — left of `Phase:`, left of every owner token and every action row — wearing a wall-clock stamp, a level word and `key="value"` grammar, and it wraps mid-word because nothing indents it. `packages.rs`'s installed-state re-read warned `cannot re-read installed packages; installing the planned set in full` with an `error =` field byte-for-byte equal to the row two lines below it, and the line sat in the scrollback for the last two minutes of a recorded run. Its plan-side twin in `plan.rs` was the same event on the same failure. Both are `debug!` now: the manager is the only field they carried that the printed row does not, and the row's own subject names it.

## The two mechanisms that keep tracing off the live region

One is the writer, in `output/`; the other is the default filter, which lives where the flags are parsed. Nothing outside `output/` may build a `MakeWriter` of its own, and nothing but `main.rs` picks a default filter.

- **`output::LiveTracingWriter`** (`output/tracing_writer.rs`) — the `MakeWriter` the binary installs. Every event is written through the printer's `MultiProgress`, so it clears the bars, lands, and lets them repaint beneath it. `main.rs` builds one before the subscriber and `attach(&printer)`s once the process printer exists; unattached, it writes plain stderr. The `every_subscriber_writes_through_a_folding_writer` fence (`output/tests/fences.rs`) keeps it the writer every subscriber takes, with a POSITIVE predicate — a refusal list grown one name at a time had passed `.with_writer(std::io::stdout)`, exactly what `fmt::Layer::default` takes. It catches a writer that is not this one (judged at its argument), a construction naming no writer, and a folding wiring missing `.with_ansi(false)`; a binding resolves through its INITIALIZER, never its name. A non-terminal writer, or a formatter whose serializer is its own sanitizer, takes the `// unfolded-writer-ok: <why>` marker.
- **`crates/cfgd/src/main.rs::tracing_filter_for(quiet, verbose, daemon)`** — the default filter: `warn` by default, `-v` debug, `-vv` trace, `--quiet` error. The RECONCILE LOOP keeps `info` as its floor (`cfgd daemon` bare, `daemon run`, the SCM-launched `daemon service`, selected by `runs_reconcile_loop`), the log being its output. `daemon install` / `uninstall` / `status` are one-shot commands reporting through the `Printer` and keep `warn`. `RUST_LOG` outranks all of it.

## `LiveBarState` is shared by every renderer writing one live region

`renderer::LiveBarState` (the live-bar count plus the broken-terminal latch) is held in an `Arc` and carried through `Printer::build_derived`, a derived printer writing the SAME `MultiProgress` and sinks as its parent. A derived renderer counting its own bars starts at zero, answers the routing gate "no bar is live" while the parent's spinner paints, and raw-writes over it — which froze `cfgd sync`'s spinner, every quiet library sink being a derived printer whose `Fail` statuses, `alert()`s and `deprecation()`s survive `Verbosity::Quiet`. **Never mint a fresh `LiveBarState` for a renderer sharing an existing region.**

A claim about a stranded paint is provable on ONE surface only — `Printer::for_test_live_terminal`, the emulated screen (`shared-utils.md`, Test guards).

## The cursor is hidden for the life of the live region

The terminal cursor sat parked in the right margin of every spinner's row, on camera in every demo. `renderer::LiveBarGuard` — the ONE seam every bar (spinner, progress bar, `LiveRow`, `OutputWindow`) passes through — hides it as the count goes 0→1 and shows it as the count returns to 0, through the stderr `Writer` the `LiveBarState` carries (`output/cursor.rs`); a per-bar toggle would flash it back mid-region as the first of two overlapping bars settled. A capture sink records the escape as a `TermLike` move, so `for_test_live_terminal`'s `LiveScreen::moves()` can assert the order; the scrollback capture carries neither escape, which is what keeps every golden a golden.

A process killed mid-spinner would leave the cursor hidden, so the first hide arms a `signal-hook` action on SIGINT/SIGTERM that writes the show sequence and then emulates the default handler — unless a cooperative owner has called `output::claim_termination_signals()` first (`daemon::ShutdownSignals`, `await_shutdown_request`, `apply`'s abort handlers, the MCP listener), in which case the action restores the cursor and leaves the exit to the owner. Windows raises no POSIX signal and gets `console`'s own behaviour. Pins: `the_cursor_hides_on_the_first_bar_and_shows_when_the_last_drops` (`output/renderer/mod.rs`), `a_narrated_wait_hides_the_cursor_before_its_first_paint_and_shows_it_after_its_last` and `the_scrollback_capture_carries_neither_cursor_escape` (`output/spinner.rs`).
