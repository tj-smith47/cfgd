# cfgd Shared Utilities — `cfgd-core/src/util/`

Cross-cutting functions live in `cfgd-core/src/util/<topic>.rs`, re-exported through `lib.rs` as `cfgd_core::<name>(...)`. **Before writing any helper, check the topic file first**; a function two modules need goes in the topic matching its domain.

This file is an **INDEX**. The reasoning — why a helper exists, what breaks without it, which callers must agree — lives in the item's own rustdoc. Open it before changing a call site.

## Topic files

| Topic file | What goes here |
|---|---|
| `util/constants.rs` | API/CSI strings, k8s label keys, OCI annotations, timeouts, histogram buckets |
| `util/time.rs` | Timestamps + duration parsing |
| `util/yaml_merge.rs` | YAML deep merge + env/alias/string-vec mergers |
| `util/strings.rs` | Env-var / alias parsing + validation, shell/XML/k8s-name escaping |
| `util/paths.rs` | Config/home paths, path validation, dir copying, test-home thread-local |
| `util/fs_perms.rs` | Cross-platform symlinks, permissions, exec-bit, inode/file-index identity |
| `util/file_io.rs` | Atomic writes, file-state capture |
| `util/process.rs` | Command helpers, memo generation counter, tool seams, env reads, shutdown |
| `util/env_session.rs` | User-session live env refresh shell-outs (`launchctl`/`systemctl --user`/`setx`) |
| `util/git.rs` | Git + Sigstore/cosign command factories, repo-reference resolution |
| `util/hashing.rs` | SHA256 helpers + loose-semver parsing/satisfaction |
| `util/apply_lock.rs` | Apply / source / backup lock acquisition |
| `util/reconcile.rs` | `EffectiveReconcile` + per-module reconcile-patch resolution |
| `util/encryption.rs` | sops/age detection |
| `util/config_inputs.rs` | The config-file input recorder |

## Constants

- `API_VERSION` — canonical API version (`cfgd.io/v1alpha1`); never a string literal.
- `CSI_DRIVER_NAME` — canonical CSI driver name (`csi.cfgd.io`).
- `MODULES_ANNOTATION` — canonical annotation key (`cfgd.io/modules`).
- `LABEL_MACHINE_CONFIG` / `LABEL_DEVICE_ID` — k8s label keys; use in gateway/controllers instead of raw strings.
- `OCI_ANNOTATION_PLATFORM` — OCI manifest annotation key; use in `oci.rs` instead of the raw string.
- `PROFILE_SCRIPT_TIMEOUT` (5m) / `COMMAND_TIMEOUT` (2m) / `GIT_NETWORK_TIMEOUT` (5m) — never hardcode the durations.
- `DURATION_BUCKETS_SHORT` / `DURATION_BUCKETS_LONG` — Prometheus histogram bucket presets.

## Time

- `utc_now_iso8601()` — the ONE ISO 8601 timestamp function; never wrap it.
- `unix_secs_now()` / `unix_secs_to_iso8601(secs)` — epoch seconds, and epoch → ISO 8601.
- `iso8601_to_filename_safe(ts)` / `utc_now_filename_safe()` — strip `:-TZ` so a stamp can be a path segment; never inline the `.replace`.
- `BACKUP_TIMESTAMP_FORMAT` / `unix_secs_to_backup_stamp(secs)` / `utc_now_backup_stamp()` — the `spec.backups[]` snapshot stamp (`20260512T143025Z`), the `{timestamp}` `namePattern` variable.
- `parse_duration_str(s)` — `"30s"` / `"5m"` / `"1h"` / plain seconds → `Duration`.
- `humanize_age_since(ts, now)` — an ISO 8601 timestamp's age (`"2h ago"`); `now` is a parameter, never a clock read, so a render pins in a test.
- `is_stale_since(ts, now, threshold_secs)` — whether `ts` is older than the threshold; shares its parse-and-diff primitive with `humanize_age_since` so the two cannot disagree.
- `humanize_until(ts, now)` — the FORWARD twin (`"in 5m"` / `"due now"`); `None` on an instant already past, which a caller words as an age instead. Shares the same signed primitive, so the two cannot disagree about where an hour ends.
- `humanize_age_cell(ts, now)` / `humanize_until_cell(ts, now)` — the ONE rendering of a listing COLUMN over either twin, including its absence: a past column reads `never`, a future one `-`. An unsubtractable stamp falls back to itself, never to the absence word; the ISO 8601 instant stays in `-o json`.
- `humanize_duration_secs(secs)` — an ELAPSED span a person reads (`3600` → `1h`), for a duration that is not an instant and so has no `now` to subtract from (a daemon's uptime). At most two units; the raw seconds stay in `-o json`.

## YAML / merges

- `deep_merge_yaml(base, overlay)` — recursive YAML value merge.
- `union_extend(target, source)` — `Vec<String>` merge without duplicates.
- `merge_env(base, updates)` / `merge_aliases(base, updates)` — merge by name, later wins.
- `split_add_remove(values)` — split `&[String]` into (adds, removes); a leading `-` is a removal.

## CLI parsing / validation

- `parse_env_var(input)` / `parse_alias(input)` — parse `KEY=VALUE` / `name=command`, validating as they go.
- `validate_env_var_user_name(name)` — the one to use for USER input: shell-safety plus the reserved `CFGD_*` refusal.
- `validate_env_var_name(name)` / `validate_alias_name(name)` — low-level shape checks; prefer the user-facing one above for user input.
- `shell_escape_value(value)` — escape a value for a shell `export`.
- `xml_escape(s)` — escape `&<>"'` for XML/plist.
- `escape_control_chars(s)` — render control characters as visible `\xNN`. The ESCAPE policy of the output system's three (see `output/mod.rs`'s `//!`); reach for it on a string a command builds OUTSIDE the renderer.
- `cursor_safe(s)` (`output/mod.rs`) — the ONE renderer FOLD for text cfgd did not author. Its routed-slot inventory and the fold/escape/strip split live in that module's `//!`; `output-module.md` states the rule.
- `sanitize_k8s_name(name)` — RFC 1123 DNS label sanitization.
- `manager_family(manager)` — the registered manager name's family (`brew-cask` → `brew`), the unit a lane and every exclusion check must agree on. Never where the manager is NAMED rather than serialized.
- `PACKAGE_SCHEMA_PATHS` + `package_schema_path(path)` (`config/resolve.rs`) — the ONE table mapping a `--package` prefix onto its `spec.packages` list; never hand-match manager names.
- `EntryOwners` (`config/resolve.rs`) + `ProfileLayer::owner_token()` — which layer last wrote each env var / alias, claimed BY the merge; never re-derived by a second walk.
- `EnvOrigins` + `ManagerPathDir` (`reconciler/env_engine.rs`) — the provenance half of a generated env file (each line's trailing `# kind:name`). Fold BOTH sides of a deployed-vs-rendered comparison through `without_owner_comment`. The PATH line's `# manager:brew,cargo` comment is a WIDER vocabulary than `OwnerKind`'s — a comma list no `Owner` name may hold, so `OwnerKind::from_token("manager")` stays `None` on purpose.
- `ModuleSurfaces::of(spec)` + `script_summary()` / `hook_names()` / `script_counts()` / `script_total()` (`modules/surfaces.rs`) — the ONE tally of what a module declares. `ScriptSpec::hooks()` is its ordering authority; never list the six lifecycle hooks by hand.
- `split_module_file_resource_id(id)` (`crates/cfgd/src/cli/live_drift.rs`) — the inverse of `module_file_resource_id`, kept beside its producer so the two spellings cannot drift.
- `DecisionContents::for_decisions(resolved, rows, config_dir, owners)` + `decision_row(item)` (`reconciler/pending.rs`) — the ONE derivation of WHAT a pending source decision would put on the machine AND of the `(outranked by <owner>)` annotation, built once by the caller. Display-only; three consumers, and they must stay three.
- `merged_entry_owners(resolved, modules)` (same file) — the `owners` argument above: the layer merge's own per-entry claims with every resolved module folded on top, in the env engine's order. Never a second opinion about precedence.
- `pending_decisions_title(count)` + `MSG_ANSWER_DECISIONS` / `MSG_INCLUDE_DECLINED_DECISIONS` (same file) — the Pending Decisions section title (the count is the section's ANNOTATION, never a row of its own) and the ONE instruction for closing a decision, one wording per state. A surface listing decisions reads all three.
- `nothing_to_do_verdict(pending)` + `MSG_NOTHING_TO_DO` (`reconciler/run.rs`) — the closing line of a run with no actions, `Role` included: `Ok` only when nothing is withheld, `Pending` + `Nothing to apply — N decisions pending` otherwise. `plan` and `apply` both settle through it, so neither can report "up to date" over an unanswered item.
- `declared_decision_fingerprints(spec)` + `DeliveredItems::content_hash_for(resource)` (`reconciler/pending.rs`) and `StateStore::latest_decision_content_hash` / `set_decision_content_hash` (`state/decisions.rs`) — the per-ITEM fingerprint a source decision is keyed on. A `Notify` item is re-asked only when ITS fingerprint differs from the row's; a row with none recorded is backfilled through `SourcePolicyReview.fingerprint_backfill`, never re-asked. Never gate on a whole-source hash.
- `profile_inventory_blocks(resolved)` (`crates/cfgd/src/cli/profile/show.rs`) — a profile's inventory as named `KvPair` blocks; its two consumers differ only in section DEPTH. Never mint a second inventory renderer.
- `source_manifest_doc_sections(doc, manifest, policy, profiles_dir)` (`crates/cfgd/src/cli/source/show.rs`) — the ONE render of a config source's manifest; `policy` is derived by the CALLER, once, from the value the payload carries.
- `load_config_and_profile_module_scoped(cli, printer, module_filter, with_profile)` (`crates/cfgd/src/cli/helpers.rs`) — the `--module` isolate shared by `apply` and `plan`: loads and drains the config exactly once, and never resolves a profile for an isolated run.
- `TokenHits` + `warn_zero_match_tokens(...)` + `known_module_names(config_dir)` (`crates/cfgd/src/cli/plan_ops.rs`) — the SSOT per-token accounting for `--skip`/`--only`, pushed into `Plan::warnings` so one producer feeds the header and `-o json`.
- `RunContext` (`crates/cfgd/src/cli/run_context.rs`) — the per-invocation holder of everything a run builds at most once. Build one at a `cmd_*`'s top; never `Sync`, so concurrent phases get the resolved objects, not the context.
- `ManifestCache` + `resolve_manifest_packages_cached(spec, config_dir, cache)` (`crates/cfgd/src/packages/mod.rs`) — reached through `ctx.resolve_manifest_packages`; never process-global, a manifest being stable only for one run.
- `Platform::current()` (`platform/mod.rs`) — the ONE platform detection per process; `detect()` is for the detection unit tests only.
- `http_agent(timeout)` (`http.rs`) — the ONE `ureq::Agent` per named timeout, so pooled TLS connections survive; a caller needing a different agent SHAPE builds its own.
- `format_bytes(bytes)` — the workspace's ONE human byte-size renderer; never hand-roll a second scale.
- `Theme::PRESET_NAMES` + `Theme::preset(name)` (`output/theme.rs`) — the ONE preset list and lookup; `cli::resolve_theme_config(path, preset)` is the ONE composition of a printer's theme block.
- `Theme::arrow()` / `Printer::arrow()` — the ONE arrow glyph for a rendered `old -> new` relationship; a pure `Doc` builder takes an `arrow: &str` parameter instead. A REVERSED relationship is reworded with "from", never a mirrored glyph.
- `Absence` (`NotInstalled` / `Missing` / `NotFound`) — the ONE absence vocabulary, chosen by WHAT is absent, not how badly. Two consumers turn its literals into WIRE values.
- `drift_kind_label` / `is_shell_drift_kind` / `drift_item_subject` / `drift_operands` / `drift_terse_cause` / `env_file_row_is_redundant` (`output/mod.rs`, beside `drift_detail`) — the drift report's VOCABULARY to `drift_detail`'s RENDERING. All six DISPLAY-only: every stored, hashed or `-o json` string keeps its producer's literal.
- `MergedEnvItems::display_values(kind, id)` (`reconciler/verify.rs`) — supplies that pair for the one kind whose STORED operands are opaque markers. Build the merge once per command; never call it on a value about to be persisted.
- `reconciler::primary_env_file(home)` — the primary managed env file on this host, and the ONE public spelling of that platform split; a consumer minting its own seeds a basename the real verifier never reads.
- `FoldedPath` + `fold_path_line(...)` / `primary_folded_path(...)` (`reconciler/env_engine.rs`) — the ONE `PATH` line a generated env file carries, over BOTH producers (the declared `spec.env` entry and the bootstrapped manager directories) and BOTH readers (the write and the per-item comparison). A dialect renders the parts through `FoldedPath::value(quote, inherited, separator)`; it never decides which entries or in what order, and a second `export PATH=` in one file is the bug this exists to make unwritable.
- `module_status_display(stored, drifted)` + `MODULE_STATUS_INSTALLED` / `MODULE_STATUS_ERROR` (`state/types.rs`) — the ONE derivation of the word a person reads for a module's state. `Drifted` is DERIVED, so a recorded-state surface passes `drifted: false`.
- `ENV_SESSION_RESOURCE_ID` (`state/types.rs`) — the recorded `resource_id` of the live-session env surface; never the bare `"refresh"` literal, which two readers spelled independently.
- `Action::pre_skip_reason()` (`reconciler/types.rs`) — why an action cannot run on THIS host, answered while the plan is read. The ONE seam: `Phase::action_count` excludes what it names and `render_plan_tree` settles it, so the plan's promise and the apply's tally are one number.
- `reconciler::attempted_count(actions)` (`reconciler/types.rs`) — the ONE spelling of that exclusion as a COUNT, over any action iterator. `Phase::action_count` (and through it `Plan::total_actions`) counts a whole plan with it; a caller holding a scoped subtree — `build_plan_output` over `in_scope_tree` — counts the subset it LISTED with the same one. Never hand-roll `.filter(|a| a.pre_skip_reason().is_none()).count()`, and never price a `--phase` payload off the unfiltered plan.
- `backup::outcome_role(clean, produced_artifact)` + `outcome_detail(error, size)` (`backup/mod.rs`) — the three-way role and the one detail slot every completed backup-engine outcome settles as. `report_backup_record` and `report_restore` are the two mutating verbs of one command and both read them, so a fourth outcome cannot be worded twice. The error leads the detail; the size joins it in parentheses.
- `ApplyStatus::{as_str, display_str, human_str}` (`state/types.rs`) — the stored / `-o json` / human spellings of an apply's outcome, pinned as a wire contract.
- `ApplySummary` + `::to_column()` / `::prose(stored)` (`state/types.rs`) — the ONE typed shape of the `applies.summary` column and the ONE prose rendering every human surface reads it back through. Never build the column with `json!`, and never print the stored string.
- `ApplyResult::{succeeded, skipped, failed}` + `RunTally.skipped` (`reconciler/`) — a successful action that CHANGED nothing is skipped, not done. Every count comes from these; `!failed` is not a success count.
- `PackageSchemaPath::noun()` + `DEFAULT_PACKAGE_NOUN` (`config/resolve.rs`), reached from the CLI through `PackageRef::noun[_capitalized]` — the word a confirmation line calls a `--package` entry (`tap` / `cask` / `package`), read off the schema path so the add verb and the remove verb cannot disagree.
- `ActionNote::next_step(message)` (`providers/mod.rs`) — a caveat that is an INSTRUCTION rather than a report. Renders as a hint, after every report in its group; `warn` / `info` stay for what the run has to say about itself.
- `cli::status::recorded_module_tallies(items, declared)` → `ModuleTally { packages, files, scripts }` — what the Managed Resources table says this host manages per module, one slot per module-owned kind the Type column spells (`env` is cfgd's own), and the ONE source of the Modules headline's counts. A headline derived from resolution disagrees with the table under it; a kind with no slot vanishes from the headline while its rows stay in the table, which is why the tally is a named struct and why `every_module_owned_kind_the_table_lists_has_a_slot_in_the_headline` walks `display_type`'s own arms. A recorded `script` row carries no count of its own, so its number comes from the `declared` entry the table cell reads.
- `BackupRunKind` (`Run` / `Safety`) (`state/types.rs`) — what wrote a `backup_runs` row, so a restore's safety copy is not read as the unit's last backup. The LEDGER holds both kinds; `latest_backup_run` filters to `Run`. `::display_str()` is the ONE word a human surface calls the kind.
- `ApplyRun::unplanned(ctx, actions)` + `RunContext::subject` (`reconciler/run.rs`) and `backup::RESTORE_ACTION_COUNT` — the plan-less run: a command whose BODY it renders itself still takes the skeleton's header and rollup, and `actions` is the one number both ends state. `subject` is the title's value half (`Restore: notes`), never a row. Never a synthesized empty `Plan`, and never a second `Config`/`Profile`/`Sources` renderer.
- `RunTitle` + `::as_str()` (`reconciler/run.rs`) — what a run calls itself: the heading it prints AND the noun its rollup lines are built from, so `Restore` cannot head a block whose closing line says `Backup`. A new run kind adds a variant here, never a literal at a call site.
- `backup::report_restore(printer, outcome)` — the ONE render of a restore's rows and rollup, returning the `RunTally` its caller reports. Never re-word a restore's outcome beside it.
- `reconciler::report_align_width(plan, filter)` + `Printer::report_column(width)` — ONE alignment column per REPORT, not per phase: the width is measured over every action the run will print (`in_scope_tree`, `PhaseCoverage::Rendered`), claimed by whoever can see the whole report, and released when the guard drops. `SectionGuard::live_column` prefers a claimed report column over its own, so a pseudo-phase (backups, `cfgd:env`) lands on the same x position as the plan's phases. A caller measuring one phase makes the detail column jump mid-report.
- `output::renderer::{action_subject_style, action_detail_is_muted}` — the ONE emphasis mapping for an action row's two halves, read by all three painters (`Printer::action_status`, `SectionGuard::action_status`, `LiveRow::set_action_status`) plus `emit_action_line`. A WITHHELD role (`Pending` / `Skipped`) holds its subject back and lets the reason speak: subject muted, detail bright. Never decide either half at a call site — that is how one row inverted between the plan tree and the apply tree.
- **Owner-carrying composers** (`output/`) — `Printer::heading_owner_prefixed`, `section_owner[_or_collapse]`, `SectionGuard`'s pair, `Doc::section_owner` / `subsection_owner`. Reach for the one matching the call site's shape instead of a hand-built `format!("{kind}:{name}")`; cataloged in `output-module.md`.
- `Printer::command_list(pairs)` and its `SectionGuard` / `Doc` / `SectionBuilder` counterparts — a "command — description" list, for any two-column list whose left side NAMES a thing; never for an ordinary key/value fact. Cataloged in `output-module.md`.
- `CommandPair::typed(key, type_span, value)` / `KvPair::{annotated, nested, role_valued}` / `TitleLabel::typed` (`output/component.rs`) — the renderer-owned styling and layout slots. A caller never paints or indents one itself; cataloged in `output-module.md`.
- `Doc::paragraph(text)` (`output/doc.rs`) — a prose paragraph for what a documentation surface says ABOUT the heading above it; cataloged in `output-module.md`.
- `AccentHeading` (`output/accent_heading.rs`, `pub(super)`) — the ONE composer for the "Caveats" heading; deliberately not a `PhaseLabel`, which would render `Phase: Caveats`.
- `Printer::narrate(running, |sp| work)` / `Printer::narrate_silent(...)` — the settle-safe spinner wrappers; never hand-roll guard + spinner + finish. Which one a wait takes is decided by who else SAYS the failure. Cataloged in `output-module.md`.
- `pluralize(count, noun)` / `plural_noun(count, noun)` / `agreeing_verb(count, verb)` — the ONE agreement rendering for a counted sentence. Regular English only; an irregular plural or `be`/`have` is spelled out at the call site.
- `sentence_case(word)` — capitalize the first character and leave the rest alone, for a stored lowercase token opening a rendered line (a decision's tier). Never a title-caser: it touches one character, so an acronym or an already-cased word survives.
- `yes_no(Option<bool>)` — the ONE `yes` / `no` / `-` rendering of a tri-state fact in a table cell. `None` reads "not known", never "no".
- `last_sync_display(last_fetched, now)` + `SOURCES_SECTION` (`crates/cfgd/src/cli/source/list.rs`) — the ONE human rendering of a config source's last fetch (`humanize_age_since`, falling back to the stamp itself, `never` when there is none) and the ONE noun for the section listing sources. Every surface showing either — `source list`, `source show`, `status`, `daemon status`, `doctor` — reads from here; the `-o json` payload keeps the ISO 8601 instant.
- `sources_table(entries, wide, now)` + `configured_source_entries(cfg, state)` (same file) — the ONE `Sources` table and the ONE derivation of the declared catalog it renders. A surface holding live facts (`daemon status`) merges them OVER a catalog row rather than building a narrower table of its own, so no two surfaces name the same source with different columns. `both_sources_surfaces_render_through_the_one_table_builder` fails on a hand-built one.
- `source_failure_next_step(err, name)` + `subscription_knob_label(key)` (`crates/cfgd/src/cli/source/mod.rs`) — what a reader DOES about a refused source, per error kind, and the rendered label for a subscription knob's wire key. Both display-only: the stored token and the `-o json` field keep the wire spelling.
- `heal_drift_hint(module)` (`crates/cfgd/src/cli/mod.rs`) — the next step a drift REPORT closes on, scoped as the report was. Distinct from `MSG_RUN_APPLY`, which invites a preview of changes the reader has not seen; every verdict surface that FOUND drift reads this one.
- `answer_decisions_hint(pending)` (`reconciler/pending.rs`) — `MSG_ANSWER_DECISIONS` with the bulk form folded in, for a surface that knows the count. The bulk half appears only above one item, where `--all` does something naming the resource cannot.
- `head_signature_accepted(name, repo_dir)` (`sources/mod.rs`) — whether the checked-out HEAD of a source carries a signature cfgd would ACCEPT, read through `verify_head_signature`'s own classifier so display and verification cannot disagree. `None` is "cannot say", which is why the column renders `-` rather than `no`.
- `action_display_subject(action)` (`reconciler/format.rs`) — the ONE display derivation of an action's subject; a preview, an alignment column and an executed line must be one string. Never persist a `DisplaySubject`.
- `script_run_subject` / `module_script_subject` / `hook_script_subject` / `bare_script_subject` (same file) — the partial views for callers holding a script's parts rather than an `&Action`; never rebuild `"{marker}: {body}"` by hand.
- `condense_action_desc_for_display(action, desc)` (same file) — the narrower gate for a raw description string that is not an action subject; never apply it to a value you persist.
- `system_resource_key(configurator, key)` (same file) — the ONE composition of a system setting's `<configurator>.<key>` identity; three surfaces mint and match it, so a byte of divergence records unresolvable drift.
- `system_key_doubling_error(configurator, key)` (same file) — the ONE statement of the no-self-prefix rule and its diagnostic; never restate the check or the message at a third site.
- `compliance::snapshot_content_hash(snapshot)` — the ONE serialize-and-digest for a compliance snapshot, dropping the volatile timestamp so an unchanged machine hashes the same twice.
- `CFGD_BACKUP_SUFFIX` / `cfgd_backup_path(target, extra)` / `backup_file(target)` (`reconciler/sidecar.rs`) — the ONE spelling and ONE writer of the sidecar cfgd leaves beside an adopted target, with `SidecarOutcome::detail()` the ONE wording. Never compose the name, the copy, or the sentence at a call site.
- `Reconciler::backing_up(targets)` — the targets a conflict settled as `Backup`, copied aside as the displacing action EXECUTES; both file-writing paths route through `back_up_adopted_target`.
- `is_unmanaged_file` / `sweep_unmanaged_file_targets` / `apply_conflict_policy` / `sweep_label` / `ResolvedConflict` / `UNMANAGED_SKIP_REASON` / `unmanaged_conflict_error` (`reconciler/adopt.rs`) — the ONE classification of "does this target hold a file cfgd never wrote", and the ONE non-prompting sweep. The CLI keeps only the PROMPT.
- `mark_unmanaged_drift(record, strategy, config_dir, state)` + `UNMANAGED_DRIFT_CAUSE` (same file) + `FileDriftResult.unmanaged` — the READ-side half: a drifted finding on a target cfgd never wrote is a different problem with a different fix. All four producers mark.
- `effective::effective_file_strategies(profile, modules, config_dir, default)` — where a producer holding only a target looks its RESOLVED strategy up; three consumers each applying their own `unwrap_or(default)` is how the sweep and `diff` disagreed.
- `oci::artifact_platforms(reference)` (`cfgd-core/src/oci/pull.rs`) — the platforms an already-pushed artifact declares, read off its manifest alone (no blob downloaded). Reads both shapes `push_module` and `push_module_multiplatform` write; an artifact declaring neither answers an empty list, never an error. The operator reaches it through `ArtifactPlatformReader`, never directly, so a controller test names no registry.
- `ModuleStatus::signature_verdict(verified, declared)` + `SIGNATURE_VERIFIED` / `SIGNATURE_UNVERIFIED` / `SIGNATURE_UNSIGNED` / `SIGNATURE_UNKNOWN` (`cfgd-crd/src/lib.rs`) — the ONE four-word vocabulary a person reads for a module's signature. The `SIGNATURE` printer column binds to the controller-written `status.signature`; `kubectl cfgd status` reads that field and derives the same verdict only for a Module no reconcile has written yet. `status.verified` stays on the wire as the raw bool. `signature_verdict` is the door for a caller holding only the two bools; `unknown` — the check never ran — is not expressible from them and is named directly by the controller that holds the real outcome.
- `oci::check_signature(reference, opts)` → `SignatureCheck` (`Valid` / `Rejected` / `Undetermined`) (`cfgd-core/src/oci/sign/mod.rs`) — `verify_signature` as a three-way verdict, for every caller that DISPLAYS or records the outcome. Turning a bare `Err` into "unverified" claims cosign rejected the artifact, which a missing cosign or an unreachable registry does not support; `verify_signature` itself stays right where any failure is fatal.
- `OciReference::uses_plain_http()` (`cfgd-core/src/oci/mod.rs`) — whether cfgd reaches this registry over HTTP (loopback, or named in `OCI_INSECURE_REGISTRIES`). `api_base` and cosign both read it: cosign opens its own connection and only treats loopback as plain HTTP unaided, so `oci/sign`'s `apply_registry_scheme` passes `--allow-insecure-registry` on every cosign subcommand. `every_cosign_subcommand_this_module_spells_declares_the_registry_scheme` walks the population.
- `ArtifactFactsReader` + `ArtifactVerifier` + `RegistryBackoff` (`cfgd-operator/src/controllers/mod.rs`) — the two seams a Module reconcile reaches a registry through, and the memo that keeps a failed visit from being repeated on the next watch event. Every component that READS a registry (operator, CSI driver, agent) also carries an `extraEnv` in `chart/cfgd`; `every_registry_reading_component_exposes_the_same_registry_knob` walks the chart.
- `Reconciler::recording_scope(scope)` — what the run's `applies` row records in place of the profile name, for a `--module` run with no profile to name. The stored column stays spelled `profile`; `cli::status::derivable_profile` is the ONE read of it.

## Shell quoting (generated shell files)

Quoting is per-dialect; there is no one correct escaper. Every `*_quoted` helper returns the COMPLETE token **including its quotes** — that is what makes a bare interpolation into `alias {}={}` impossible to write by accident.

- `posix_double_quoted(value)` — a complete `"…"` token for bash/zsh.
- `escape_double_quoted(s)` — the BODY of the above, for a caller joining escaped fragments inside one pair of quotes. A plain `$NAME` / `${NAME}` still expands; `$(cmd)`, `$((…))` and every `${x…}` form become literal.
- `posix_single_quoted(value)` — a complete `'…'` token for POSIX sh, and the correct quoting for a systemd `environment.d` assignment.
- `fish_single_quoted(value)` — a complete `'…'` token for fish.
- `powershell_single_quoted(value)` — a complete `'…'` token; fully literal, and the default choice for PowerShell.
- `powershell_double_quoted(value)` / `escape_powershell_double_quoted(s)` — the interpolating pair, for a declared value carrying `$env:` references.
- `cmd_double_quoted(value)` — a complete `"…"` token for `cmd.exe`/batch, doubling `%`.

A PowerShell function-wrapper alias carries its command as a quoted string built into a script block at CALL time (`function n { & ([scriptblock]::Create('<cmd> @args')) @args }`) — pasted between the braces, a `}` closes the function early.

## Filesystem

- `default_config_dir()` — cross-platform config dir.
- `expand_tilde(path)` — expand `~/` to home (`HOME`, then `USERPROFILE` on Windows).
- `normalize_path_entry(entry, home)` — fold ONE `PATH` entry to the form two spellings of the same directory compare equal in (`$HOME`/`${HOME}`/`~` resolved, separators folded, trailing `/` dropped). COMPARISON only; never render the result. Every env dialect's derived manager-PATH line drops a directory the declared `PATH` already carries through this one key.
- `absolutize_path(path)` — make a path absolute LEXICALLY without requiring it to exist; use at any CLI entry point. Never canonicalizes, so a symlinked config keeps the name the user gave it.
- `resolve_relative_path(path, base)` — resolve relative to base with traversal validation.
- `resolve_managed_file_source(source, config_dir)` — the ONE resolution of a `spec.files[].source` against the config dir, taken by BOTH readers of that field.
- `validate_path_within(path, root)` — canonicalize and verify containment.
- `validate_no_traversal(path)` — reject a reference containing `..` or naming nothing of its own; use for any path cfgd reads or writes.
- `validate_plain_name(raw)` — stricter, judged on the RAW string. Use for any string that NAMES something cfgd creates under a root it may later delete or mount wholesale; Windows shapes are rejected on every host, so a name is valid everywhere or nowhere.
- `atomic_write(target, content)` / `atomic_write_str` — atomic temp+rename write returning the SHA256; use instead of `fs::write` in ALL production code. Replaces a symlink at the target rather than following it.
- `atomic_write_merged(target, content)` — the `strategy: Patch` write: resolve a symlink first, so the target keeps its mode and its link identity.
- `atomic_write_resolved[_str](target, content)` — the FOLLOW-the-symlink variants, for a user-owned file cfgd did not author where a stow/chezmoi link must survive. A dangling link is written at the link path itself.
- `ensure_parent_dir(target)` — create a file's parent; use instead of the inline `if let Some(parent)` idiom. For a named directory, call `create_dir_all` directly.
- `write_scaffold(kind, path, body)` (`crates/cfgd/src/cli/helpers.rs`) — scaffold writes in the binary crate: the modeline pinned to the BINARY's version, plus an atomic write. Rewrites of user-owned files must not use it.
- `rewrite_user_yaml(path, &value)` (same file) — rewrites of user-owned YAML: re-prepends the leading comment block, prunes absent sections and undeclared scalar defaults. Use instead of raw `to_string` + `atomic_write_str`; it is why no field carries `skip_serializing_if`.
- `quoted_assignment(name, value)` (same file) — the ONE rendering of a declared env var or alias as `name="value"`, for every surface that shows one (the `Set env` / `Set alias` confirmations, `status <module> --show-values`). Quoted UNCONDITIONALLY through `posix_double_quoted`, the same quoter the generated env file's `export`/`alias` lines are written with, so the line reporting a write and the file it wrote spell one assignment one way; a value holding a space read unquoted is a different value (`catn=cat -n` reads as the alias `cat`). NOT for a module-review surface (`module registry`'s pre-approval listing), which must show the declared bytes rather than an escaped form, and not for the `Removed env` / `Removed alias` halves, which take a bare key. `every_assignment_a_setter_confirms_renders_through_the_one_quoter` walks the population.
- `copy_dir_recursive(src, dst)` — recursive tree copy; correct ONLY where cfgd owns the destination.
- `carry_dir_mode(src, dst)` — best-effort directory mode copy; call it AFTER populating `dst`.
- `create_symlink(source, target)` — cross-platform; Windows errors with Developer Mode guidance.
- `is_same_inode(a, b)` — same file, two paths, one moment.
- `file_identity(path)` — that identity as ONE value, captured now and compared later by a holder that must notice its path being re-pointed. `None` reads as "cannot say", never "different".
- `try_file_identity(path)` — the same probe REPORTING why it failed; reach for it wherever "not the file I opened" and "I could not look" must lead to different actions.
- `file_permissions_mode(metadata)` / `set_file_permissions(path, mode)` / `is_executable(path, metadata)` — Unix mode bits and exec-bit; no-ops or extension checks on Windows.
- `capture_file_state(path)` / `capture_file_resolved_state(path)` / `FileState` — content, hash, permissions and symlink state, unfollowed and followed.

## Process / commands

The **30-second memo convention** and the exclusion a TTL guard needs are in `util/process.rs`'s and `test_helpers.rs`'s module docs. Read them before adding a memo or a pin.

- `command_available(cmd)` — is this command on PATH; the `is_some()` view over `command_path`.
- `command_path(cmd)` — resolve a command to its executable path, memoized per name (misses included). Searches `$PATH` then the bootstrapped directories.
- `command_resolution_generation()` / `invalidate_command_resolution()` — the ONE counter behind the path and availability memos. Any new path that puts a binary on the machine, or takes one off it, calls the invalidator.
- `ProviderRegistry::package_managers()` / `system_configurators()` + `add_*` / `extend_*` / `set_package_managers` (`providers/mod.rs`) — the read and write halves of two PRIVATE vectors; every mutator retires its own availability sweep. Never widen the fields back to `pub`.
- `PackageContext::installed_for(manager)` (`providers/installed.rs`) — the ONE question "what does this manager report installed". Never call `installed_packages[_with_versions]` from a caller holding a context; a command reading twice threads ONE context.
- `ProviderRegistry::manager_map()` — the ONE `name → &dyn PackageManager` map every module-resolution entry point takes; keyed by REGISTERED name, never the family.
- `PackageManagerExt::available_version_memoized(package)` (`providers/available.rs`) — the ONE question "what does this manager currently OFFER". Never call the trait's `available_version` from a resolution or display path.
- `SecretCache` (`providers/secret_cache.rs`) — the ONE secret resolution per `(backend, reference)` per RUN. Not process-scoped and no TTL: it holds plaintext, so its lifetime is the unit of work.
- `Reconciler::fill_planned_versions(modules, managers)` (`reconciler/plan.rs`) — prices a plan's SURVIVING packages; **every caller of `Reconciler::plan` fills through it on the same reconciler**, the description being persisted as well as rendered.
- `modules::fill_available_versions(packages, managers)` — the unconditional per-package form for the two surfaces that print a version without planning (`doctor`, `module show`); no planning path may call it.
- `command_output_with_timeout(cmd, timeout)` — run a `Command` with a timeout, killing on overrun. It OWNS the stdio configuration; never set stdio yourself.
- `terminate_process(pid)` — SIGTERM / TerminateProcess.
- `exit_status_reason(status)` — the ONE rendering of why a child ended; never `status.code().unwrap_or(-1)`, which names an impossible code while hiding the signal that explains it.
- `stdout_lossy_trimmed(output)` / `stderr_lossy_trimmed(output)` — trimmed lossy-UTF8 capture.
- `is_root()` — euid==0 / `IsUserAnAdmin()`.
- `hostname_string()` — system hostname; `"unknown"` on failure.
- `tracing_env_filter(default)` — `EnvFilter` from the environment with a fallback.
- `env_or(var, default)` — read an env var with a fallback; the ONE spelling for the two server binaries. A read that must WARN on a bad value belongs beside the operator's own parsers.
- `await_shutdown_request()` — the ONE SIGINT+SIGTERM registration-and-select for a server binary (`#[cfg(unix)]`); a caller adds logging and never its own handler. `daemon::ShutdownSignals` is deliberately separate.
- `require_tool(name, install_hint)` — the uniform "X not found" error for every `command_available`-gated flow.
- `tool_cmd(env_var, default)` — the generic seam-honouring `Command` factory.
- `systemctl_cmd()` / `systemctl_available()` / `SYSTEMCTL_BIN_ENV` — the ONE `systemctl` factory, predicate and seam. Never `Command::new("systemctl")` (unshimmable and unbounded) or `command_available("systemctl")` (answers from PATH while the spawn answers from the seam).
- `session_manager_available()` / `NO_SESSION_MANAGER` — whether THIS host has a live-session environment manager (`setx` / `launchctl` / `systemctl --user`), and the ONE wording for its absence. The plan, the apply's skip detail and `status`'s session row all answer from it, so no two surfaces can disagree about whether the publish can happen.
- `reg_cmd()` / `REG_BIN_ENV` — the same for the Windows registry, shared by the session-env refresh and the `windowsRegistry` configurator. The registry is not a path, so redirecting the BINARY is a test's only sandbox.
- Keyed system configurators name their own seams beside their `tool_cmd` factories (`CFGD_GSETTINGS_BIN`, `CFGD_XFCONF_QUERY_BIN`, `CFGD_KREADCONFIG_BIN` / `CFGD_KWRITECONFIG_BIN`, `CFGD_DEFAULTS_BIN`; `windowsRegistry` reuses `CFGD_REG_BIN`). They gate availability differently — seam-answered for `gsettings`/`xfconf`/`kdeConfig`, platform-gated for `macosDefaults`/`windowsRegistry`. Drive them through `test_helpers::ToolShim`.
- `register_bootstrapped_path_dirs(dirs)` — make the PATH directories cfgd created THIS RUN visible to later resolutions; never `set_var("PATH", …)`, unsound once any thread is live.
- `bootstrapped_path_dirs()` — a snapshot of that registry.
- `path_with_dirs_prepended(current, dirs)` — the ONE composition of a PATH whose leading entries are `dirs`; `None` when the value would not change, so a caller leaves the environment alone.
- `process_path_with_dirs_prepended(dirs)` — the same over THIS process's PATH, and what every consumer outside cfgd-core calls, so the read takes the same guard every production reader does.
- `restore_bootstrapped_path_dirs(dirs)` — test-only rewind; reach for it through `BootstrappedPathDirsGuard`.

## Git

- `git_cmd_safe(url, ssh_policy)` — a `Command` for git with `GIT_TERMINAL_PROMPT=0` and configurable host-key checking; required for anything that may touch a remote.
- `git_cmd_local()` — the LOCAL-only factory (`config`, `tag -v`, `add`, `commit`, `rev-parse`, `log`). Use instead of `Command::new("git")` for every local invocation.
- `try_git_cmd(url, args, label, ssh_policy)` — run via `git_cmd_safe`, `true` on success; the CLI-first fallback before every git2 network operation, preventing SSH hangs.
- `resolve_repo_reference(value)` — the ONE resolution of a user-written repository reference, and what EVERY user-facing entry point calls: `acme/config` is both a shorthand and a relative path, and only the filesystem can say which was meant.
- `expand_github_shorthand(value)` — the ONE `owner/repo` → GitHub URL expansion, answered from the STRING alone; reach for `resolve_repo_reference` in any command path.
- `detect_default_branch(repo_dir)` — best-effort `origin/HEAD` then local `HEAD`.
- `detect_git_remote()` / `detect_git_head()` — the CWD repo's origin URL and HEAD SHA; use for artifact provenance instead of re-deriving.
- `git_ssh_credentials(url, username, allowed)` — the git2 credential callback (SSH agent + HTTPS helper).
- `fetch_git_source(git_src, cache_base, module_name, printer)` (`modules/git.rs`) — the ONE materialization of a module's git source, and the only thing deciding whether a transfer is needed. Its two short-circuits must never be re-derived at a call site.
- `is_git_source(value)` (`modules/git.rs`) — the ONE git-URL predicate, pure and scheme-based, deliberately never probing the filesystem. `is_clonable_source` layers `--from`'s extra arms rather than widening it.

## Sigstore / cosign

- `cosign_cmd()` — the ONE cosign factory; consumers add the subcommand and flags. Use instead of `Command::new("cosign")` anywhere Sigstore work is shelled out.
- `oci::COSIGN_PREDICATE_TYPES` + `oci::attestation_type_name(uri)` — the ONE fold between the WIRE vocabulary (the `predicateType` a manifest annotation records) and the FLAG vocabulary (`cosign verify-attestation --type`). Every surface naming an attestation type reads the right column; an unknown predicate is reported verbatim, which is also what `--type` takes for it.
- `oci::artifact_facts(reference)` → `ArtifactFacts { platforms, attestations }` — the ONE registry visit answering both "what platforms does this artifact declare" and "what attestations hang off it" (the cosign `.att` tag). One read, so a status can never mix two visits; the subject manifest is the fallible half, and an unreadable `.att` reads as no attestations.

## Hashing / versions

- `sha256_hex(data)` / `sha256_digest(data)` — SHA256 as hex, and as an OCI-style `sha256:<hex>`; use instead of inline `Sha256::digest`.
- `strip_sha256_prefix(s)` — strip `sha256:`; idempotent.
- `short_commit(commit)` — the 12-char display form of a commit id; every human surface naming a commit (`source show`, `sync`, the daemon sync log) renders through it, persisted/`-o json` ids stay full.
- `Sha256Stream` — the incremental form (`update`, `absorb_file`, `finish_hex` / `finish_digest`), for a digest over many inputs one of which is a file. The seam order IS the digest: never reorder a caller's parts.
- `parse_loose_version(s)` — 1/2/3-part version → semver `Version`.
- `version_satisfies(version, requirement)` — semver range check.

## Locks / reconcile

- `acquire_apply_lock(state_dir)` — the exclusive apply lock; returns an RAII guard.
- `acquire_source_lock(cache_dir, on_wait)` — the source-cache mutex, and the one lock that BLOCKS rather than refusing. Never the apply lock; `on_wait` announces through `printer.alert`, never `status_simple`. **Nothing in cfgd ever deletes a lock file.**
- `sources::discard_cached_checkout(cache_dir, name, printer)` — the ONE deletion of a source's cached checkout, holding the source lock; never a bare `remove_dir_all`.
- `resolve_effective_reconcile(module, profile_chain, config)` / `EffectiveReconcile` — per-module reconcile settings resolved from patches, with no `Option`s left.

## Config inputs (what a derivation READ)

- `record_config_input(path)` — the ONE report that a file or listing was consulted while deriving typed config, called from the READ sites so a caller never GUESSES the file set.
- `ConfigInputRecorder::start()` / `.finish()` — the RAII frame; frames nest, so an inner recorder does not steal an outer one's entries.
- `ConfigInputs::unchanged()` — re-stat every entry. An EMPTY set answers `false`: a derivation that recorded nothing has nothing that could report it stale.
- `daemon::tick_cache::TickCache` — the daemon's holder built on the above, and the shape a second long-lived holder reaches for rather than minting a new fingerprint scheme.

## Encryption

- `is_file_encrypted(path, backend)` — sops (`sops.mac` + `lastmodified`) or age header detection.

## Snapshot normalizers

Plain `cfgd_core::*` exports from `util/paths.rs` that make a captured render host-stable. Snapshot tests reach them through `normalize_for_snapshot`; call one directly only for a single fold.

- `normalize_for_snapshot(captured, &[(path, label)])` — the composed entry point: `\`→`/`, CRLF→LF, and each path substituted with its label.
- `normalize_cfgd_version(s, version)` — substitute the EXACT running version, so a wrong version still fails to match.
- `normalize_snapshot_durations(raw)` — replace every ` (N.Ns)` elapsed suffix with ` (XXs)`; never re-implement the scan, or two suites disagree about what counts as timing.

## Test guards

Reached via `cfgd_core::test_helpers::*`, gated behind the `test-helpers` Cargo feature. Pair every env-var consumer with `serial_test::serial`; which exclusion each TTL guard needs is in that module's doc.

- `BootstrappedPathDirsGuard::capture()` / `::capture_and_clear()` — RAII snapshot+restore of the bootstrapped-PATH registry, REQUIRED in any fixture driving a bootstrap. Emptying `PATH` is not sufficient for a "not found" branch; use `capture_and_clear`.
- `path_env_read_guard()` / `path_env_mutation_guard()` — the gate over the process-global `PATH`. A mutating test takes the WRITE guard, declared before its `EnvVarGuard`; a test asserting a SUCCESSFUL resolution takes the read guard. Never spawn while holding the write guard.
- `await_queued_path_writer(timeout)` — blocks until a writer is queued; the observable a concurrency test needs instead of a sleep.
- `await_blocking_source_acquire(timeout)` — the same observable for the source lock's blocking arm. Wait on this, never on `on_wait`, which fires BEFORE the acquire.
- `CommandPathMemoTtlGuard::{never_expires, always_expired, pinned}` — RAII pin of the `command_path` TTL; needs no serialization, its users asserting on an ANSWER a pin cannot change.
- `AvailableVersionMemoTtlGuard::…` — the same for the available-version ceiling; pair with `#[serial_test::serial(available_version_memo)]`. Two fixtures claiming one manager offers two versions of one package are claiming two machines — make them agree.
- `AvailabilityMemoTtlGuard::…` — the same for the provider-availability sweep; pair with the UNNAMED `#[serial_test::serial]`, the group its own count tests share.
- `ConfigReuseMaxAgeGuard::…` / `ModuleReuseTtlGuard::…` — the tick cache's two reuse ceilings; two guards rather than one so a test can say which it is asserting about. Pair with `#[serial_test::serial(tick_cache_reuse)]`.
- `GitRefreshWindowGuard::…` — the module git-cache refresh window. The pin SERIALIZES ITSELF, so no call site can forget an attribute; it excludes pins and only pins.
- `measured_in_a_stable_generation(measure)` — run `measure` in a window where nothing else moved the resolution generation. REQUIRED by every memo-hit claim; the closure must be re-runnable, so build its subject INSIDE it.
- `captured_text(&buf)` — the ONE read of a capture buffer, ANSI-stripped. Use it for any assertion about TEXT even though captures pin colour off: a negative `!contains` goes VACUOUS the moment styling is on.
- `Printer::for_test_split_streams(verbosity)` — the split-stream capture, the one constructor that can state a stdout-purity claim directly.
- `Printer::for_test_with_theme_colored(theme, verbosity)` — the ONE capture whose buffer carries ANSI, for a test whose subject IS the escapes. To assert the colour DECISION instead, call `output::printer::colors_must_be_disabled(&format)` and render nothing.
- **The three live-region capture constructors.** They differ only in where indicatif draws, so the wrong one answers a different question SILENTLY rather than failing:
  - `Printer::for_test_live_scrollback()` — indicatif draws to a HIDDEN target, so the buffer is only what was committed permanently. For commit order and commit-exactly-once; blind to the region by construction.
  - `Printer::for_test_with_live_bars()` — indicatif draws into the SAME buffer, so it holds every paint in order. For interleaving, garbling and draw ORDER; it cannot answer "how many lines are on screen".
  - `Printer::for_test_live_terminal(rows, cols)` — indicatif draws onto an EMULATED SCREEN, cursor moves and clears executed. The ONLY surface that sees a line the region drew and never erased; it reads the VISIBLE screen, so a taller region loses evidence off the top.
- `test_printer()` — a bare Quiet `Printer` for a fixture that asserts nothing about output. NEVER `Printer::new`, which inherits the invoking terminal and hangs on an unanswered prompt under a pty.
- `EnvVarGuard::set(key, value)` / `::unset(key)` — RAII env-var save/restore, restored even on panic.
- `with_test_env_var(var, value, f)` — the scoped-closure form.
- `spawn_blocking_with_test_home(f)` — `spawn_blocking` re-installing the caller's test-home thread-local; REQUIRED for every blocking dispatch whose closure may resolve `~`.
- `ConcurrencyWitness` + `MockPackageManager::with_concurrency_witness(w)` — proof that two lanes really overlapped, so a concurrency test asserts a peak, never a wall-clock bound.
- `ProbePath::containing(&[names])` — a `PATH` of one temp dir holding exactly the named executables (Unix-only). Assert the negative under an empty `PATH` and the positive here, one binary at a time.
- `CosignTestShim::install()` / `::builder()...install()` — the fake-cosign shim (argv logging, keygen mode, exit code, canned stderr), restoring the prior seam on drop.
- `freeze_last_scan_at(&StateStore, timestamp)` — pins the recorded scan stamp and then REFUSES every later write, so a `cfgd`-crate test can drive the refused-write branch. The refusal lives in the database FILE, the consumer opening its own store.

## Upgrade

- `cleanup_old_binary()` (`upgrade.rs`) — remove the `.exe.old` left by the Windows rename-dance self-upgrade; no-op on Unix, called from `main.rs` on startup.

## What NOT to do

- Don't create new utility files outside `cfgd-core/src/util/`. Shared functions go in the existing topic file that matches the helper's domain.
- Don't add the same helper as a sibling of an existing topic file. Pick the existing topic.
- Don't create a brand-new topic file unless the helper genuinely doesn't fit any existing one — three string-validation functions don't justify a new file when `strings.rs` exists.
- Don't duplicate a function that already exists. Search this catalog first.
- Don't create local timestamp/hash/command-check wrappers — use the shared ones above.
- Don't restate an item's rationale here. This file says WHAT and WHEN; the item's rustdoc says WHY, and a second copy drifts.
