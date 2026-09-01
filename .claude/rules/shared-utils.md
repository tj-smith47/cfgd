# cfgd Shared Utilities — `cfgd-core/src/util/`

Cross-cutting functions live in `cfgd-core/src/util/<topic>.rs`, re-exported through `lib.rs` as `cfgd_core::<name>(...)`. **Before writing any helper, check the topic file first**; a function two modules need goes in the topic matching its domain.

This file is an **INDEX**. The reasoning — why a helper exists, what breaks without it, which callers must agree — lives in the item's own rustdoc (for a rule with no item, in its pinning test's doc comment). Open it before changing a call site. **An entry here is one to three sentences**: what it is, when to reach for it, at most one pin name. A fix that mints a class rule adds the ONE-line entry and puts the story in the code, never a retelling here. `.claude/scripts/audit.sh` gates every entry's byte size so this density holds.

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
- `humanize_age_cell(ts, now)` / `humanize_until_cell(ts, now)` — the ONE rendering of a listing COLUMN over either twin, including its absence: a past column reads `never`, a future one `-`; the ISO 8601 instant stays in `-o json`.
- `humanize_age_magnitude_cell(ts, now)` — the MAGNITUDE twin of `humanize_age_cell`, over the same bucketing. The slot states the magnitude when the KEY is the dimension (`Age  3m`) and the relation when the key names the EVENT (`Last Applied  3m ago`); `no_age_slot_restates_the_dimension_its_key_names` walks the population.
- `humanize_duration_secs(secs)` — an ELAPSED span with no `now` to subtract from (a daemon's uptime); at most two units, raw seconds stay in `-o json`.

## YAML / merges

- `deep_merge_yaml(base, overlay)` — recursive YAML value merge.
- `fold_env_layer(base, overlay, separator)` — the LAYER fold of `spec.env`, and the ONE place `PATH` concatenates around the ambient reference instead of being replaced, deduplicated by `normalize_path_entry`. Every layer fold reads it; `merge_env` stays the DOCUMENT-edit semantic the CLI setters need.
- `union_extend(target, source)` — `Vec<String>` merge without duplicates.
- `merge_env(base, updates)` / `merge_aliases(base, updates)` — merge by name, later wins.
- `split_add_remove(values)` — split `&[String]` into (adds, removes); a leading `-` is a removal.

## CLI parsing / validation

- `canonical_bool_str(raw)` — a boolish env-var value (`1`/`yes`/`on`/… and their negatives) to clap's canonical `"true"`/`"false"`; `None` for anything else. Shared across the `cfgd` binary's env pre-normalization and the library's own manual boolean-env reads (`cli::resolve_hints_enabled`) because the two are separate crate compilations.
- `cli::resolve_hints_enabled(config_path, no_hints_flag)` (`crates/cfgd/src/cli/mod.rs`) — the ONE precedence resolution for whether closing `→` hints render: `--no-hints` flag > `CFGD_USAGE_HINTS` env > `spec.usageHints` > default on. Called once in `main.rs`, beside `resolve_theme_config`, feeding `Printer::with_hints_enabled`.
- `parse_env_var(input)` / `parse_alias(input)` — parse `KEY=VALUE` / `name=command`, validating as they go.
- `validate_env_var_user_name(name)` — the one to use for USER input: shell-safety plus the reserved `CFGD_*` refusal.
- `validate_env_var_name(name)` / `validate_alias_name(name)` — low-level shape checks; prefer the user-facing one above for user input.
- `shell_escape_value(value)` — escape a value for a shell `export`.
- `xml_escape(s)` — escape `&<>"'` for XML/plist.
- `escape_control_chars(s)` — render control characters as visible `\xNN`. The ESCAPE policy of the output system's three (see `output/mod.rs`'s `//!`); reach for it on a string a command builds OUTSIDE the renderer.
- `cursor_safe(s)` (`output/mod.rs`) — the ONE renderer FOLD for text cfgd did not author. Its routed-slot inventory and the fold/escape/strip split live in that module's `//!`; `output-module.md` states the rule.
- `PackageListSpec` (`config/profile_spec.rs`) + `ScriptCommand` (`config/module.rs`) — the NAMED map forms of the two list-or-map unions, read by both the deserializer and the schema; a union's object arm is always a named type, or `explain` renders an unresolvable `object`.
- `sanitize_k8s_name(name)` — RFC 1123 DNS label sanitization.
- `manager_family(manager)` — the registered manager name's family (`brew-cask` → `brew`), the unit a lane and every exclusion check must agree on. Never where the manager is NAMED rather than serialized.
- `PACKAGE_SCHEMA_PATHS` + `package_schema_path(path)` (`config/resolve.rs`) — the ONE table mapping a `--package` prefix onto its `spec.packages` list; never hand-match manager names.
- `SimpleManager::install_cmd` / `update_cmd` + `manager_install_script(manager, packages)` (`crates/cfgd/src/packages/`) — the declaration table owns HOW a family installs; every surface emitting an install for a declared package (apply path, every `module export` dialect) composes from it. `every_manager_install_the_cli_emits_spells_its_weak_dependency_policy_once` walks for hand-written install verbs (`// install-verb-ok:` hatch).
- `EntryOwners` (`config/resolve.rs`) + `ProfileLayer::owner_token()` — which layer last wrote each env var / alias, claimed BY the merge; never re-derived by a second walk.
- `EnvOrigins` + `ManagerPathDir` (`reconciler/env_engine.rs`) — the provenance half of a generated env file (each line's trailing `# kind:name`). Fold BOTH sides of a deployed-vs-rendered comparison through `without_owner_comment`.
- `ModuleSurfaces::of(spec)` + `script_summary()` / `hook_names()` / `script_counts()` / `script_total()` (`modules/surfaces.rs`) — the ONE tally of what a module declares. `ScriptSpec::hooks()` is its ordering authority; never list the six lifecycle hooks by hand.
- `split_module_file_resource_id(id)` (`crates/cfgd/src/cli/live_drift.rs`) — the inverse of `module_file_resource_id`, kept beside its producer so the two spellings cannot drift.
- `module_file_spec_resource_id(module, file)` (same file) — that id from the DECLARED file alone, for a caller probing a drifted-id set before any check record exists; owns the Patch/unexpanded-target split.
- `state::package_resource_id(manager, package)` / `split_package_resource_id(id)` (`cfgd-core/src/state/managed.rs`) — the ONE composition and split of a package tracking row's `<manager>/<package>` id, kept beside `upsert_package_resource` and cross-pinned as inverses; the drift writers' `<mgr>:<pkg>` is a different grammar this pair does not own.
- `reconciler::package_drift_resource_id(manager, packages)` (`cfgd-core/src/reconciler/types.rs`, beside `action_resource_info`) — the ONE `<mgr>:<pkg>` / `<mgr>:<a>,<b>` DRIFT-row id composer for the whole workspace; the cli crate reaches it through `cli::diff`'s re-export. `every_core_minted_package_drift_id_comes_from_its_composer` and the cli walk pin both crates' mints.
- `LiveDriftReport` + `live_drift_results(...)` (same file) — the ONE full-machine live drift walk: findings, erroring checks, and the package plan they were priced from. `diff`, `status --scan` and both `--exit-code` gates consume it; never a second six-pass walk.
- **A check that could not run is a first-class row, never an abort or a silent drop** — every `--exit-code` surface renders `<key>: error checking drift — <detail>` and exits `Error` (1) ahead of `DriftDetected` (5): unknown outranks known. `SystemCheckError` (`reconciler/verify.rs`) is the one shape all three producers mint. `every_exit_code_surface_reports_an_erroring_check` walks the clap population into `tests/drift_exit_code.rs`'s matrix.
- `CfgdFileManager::sorted_managed_specs(profile)` / `diff_managed_one(managed, printer)` (`crates/cfgd/src/files/plan.rs`) — the ONE target-ordered enumeration of a profile's managed entries and the per-entry inline-diff render; a caller that already knows which entries drifted re-renders only those through them.
- `DecisionContents::for_decisions(resolved, rows, config_dir, owners)` + `decision_row(item)` (`reconciler/pending.rs`) — the ONE derivation of WHAT a pending source decision would put on the machine AND of the `(outranked by <owner>)` annotation. Display-only; three consumers, and they must stay three.
- `DecisionExclusions::withholds_recorded_row(rtype, rid)` (`reconciler/pending.rs`) — whether a recorded drift row names a resource the pending-decision prune withheld from the plan, over every id grammar both producers mint; the AGGREGATE ids only the prune can attribute come back as `WithheldFromPlan::resource_ids`. A complement-resolve reading a pruned plan consults both, or the withheld rows heal blind; `a_tick_keeps_the_rows_of_a_resource_awaiting_a_source_decision` pins the tick.
- `merged_entry_owners(resolved, modules)` (same file) — the `owners` argument above: the layer merge's own claims with every resolved module folded on top, in the env engine's order. Never a second opinion about precedence.
- `pending_decisions_title(count, scope)` / `declined_decisions_title(count, scope)` + `DecisionsTitleScope` + `MSG_ANSWER_DECISIONS` / `MSG_INCLUDE_DECLINED_DECISIONS` (same file) — the two decision-section titles (the count is the section's ANNOTATION, never a row) and the ONE instruction for closing a decision. `every_decisions_section_title_comes_from_the_one_builder` walks for hand-built literals.
- `nothing_to_do_verdict(pending)` + `MSG_NOTHING_TO_DO` (`reconciler/run.rs`) — the closing line of a run with no actions, `Role` included: `Ok` only when nothing is withheld. `plan` and `apply` both settle through it.
- `declared_decision_fingerprints(spec)` + `DeliveredItems::content_hash_for(resource)` (`reconciler/pending.rs`) and `StateStore::latest_decision_content_hash` / `set_decision_content_hash` (`state/decisions.rs`) — the per-ITEM fingerprint a source decision is keyed on; never gate on a whole-source hash.
- `list_or_struct_schema::<T>` / `list_or_packages_vec_schema` (`config/profile_spec.rs`) — the `schema_with` twins of the two widening deserializers: a schema declares every shape the deserializer accepts, because `explain`, `-o json` and the SchemaStore-published schemas read the reflection and cannot see serde. A NARROWING validator needs no twin. `every_list_or_map_package_field_declares_both_shapes_in_its_schema` walks both; re-bless via `task schema:bless`.
- **A doc comment on a `JsonSchema` type is USER documentation in full** — schemars takes the whole `///` block as the schema description, which lands on `explain`, `-o json` and the published schemas. The maintainer WHY goes in a `//` comment, never the `///` block. `no_schema_description_addresses_a_maintainer_instead_of_a_user` walks every description.
- `profile_inventory_blocks(resolved)` (`crates/cfgd/src/cli/profile/show.rs`) — a profile's inventory as named `KvPair` blocks, `Aliases` leading `Env`; its three consumers differ only in section DEPTH. Never mint a second inventory renderer.
- `source_manifest_doc_sections(doc, manifest, policy, profiles_dir)` (`crates/cfgd/src/cli/source/show.rs`) — the ONE render of a config source's manifest; `policy` is derived by the CALLER, once.
- `load_config_and_profile_module_scoped(cli, printer, module_filter, with_profile)` (`crates/cfgd/src/cli/helpers.rs`) — the `--module` isolate shared by `apply` and `plan`: loads and drains the config exactly once, and never resolves a profile for an isolated run.
- `TokenHits` + `warn_zero_match_tokens(...)` + `known_module_names(config_dir)` (`crates/cfgd/src/cli/plan_ops.rs`) — the SSOT per-token accounting for `--skip`/`--only`, pushed into `Plan::warnings` so one producer feeds the header and `-o json`.
- `RunContext` (`crates/cfgd/src/cli/run_context.rs`) — the per-invocation holder of everything a run builds at most once. Build one at a `cmd_*`'s top; never `Sync`, so concurrent phases get the resolved objects, not the context.
- `ManifestCache` + `resolve_manifest_packages_cached(spec, config_dir, cache)` (`crates/cfgd/src/packages/mod.rs`) — reached through `ctx.resolve_manifest_packages`; never process-global, a manifest being stable only for one run.
- `Platform::current()` (`platform/mod.rs`) — the ONE platform detection per process; `detect()` is for the detection unit tests only.
- `PlatformGated` + `applicable_here(entries, platform)` + `validate_platform_tag(tag)` / `deserialize_platform_tags` (`platform/mod.rs`) — the ONE gating predicate, list filter and parse-time tag validation over every type carrying a `platforms:` list; `platform_annotation()` is the display half. A gated-off entry is filtered BEFORE the layer fold, never inside it.
- `http_agent(timeout)` (`http.rs`) — the ONE `ureq::Agent` per named timeout, so pooled TLS connections survive; a caller needing a different agent SHAPE builds its own.
- `format_bytes(bytes)` — the workspace's ONE human byte-size renderer; never hand-roll a second scale.
- `Theme::PRESET_NAMES` + `Theme::preset(name)` (`output/theme.rs`) — the ONE preset list and lookup; `cli::resolve_theme_config(path, preset)` is the ONE composition of a printer's theme block.
- `Theme::arrow()` / `Printer::arrow()` — the ONE arrow glyph for a rendered `old -> new` relationship; a pure `Doc` builder takes an `arrow: &str` parameter instead. A REVERSED relationship is reworded with "from", never a mirrored glyph.
- `Absence` (`NotInstalled` / `Missing` / `NotFound`) — the ONE absence vocabulary, chosen by WHAT is absent, not how badly. Two consumers turn its literals into WIRE values.
- `drift_kind_label` / `is_shell_drift_kind` / `drift_item_subject` / `drift_operands` / `drift_terse_cause` / `env_file_row_is_redundant` (`output/mod.rs`, beside `drift_detail`) — the drift report's VOCABULARY to `drift_detail`'s RENDERING. All six DISPLAY-only: every stored, hashed or `-o json` string keeps its producer's literal.
- `MergedEnvItems::display_values(kind, id)` (`reconciler/verify.rs`) — supplies that pair for the one kind whose STORED operands are opaque markers. Build the merge once per command; never call it on a value about to be persisted.
- `env_item_verify_results(env, aliases, owners, modules)` → `EnvItemCheck` (`reconciler/verify.rs`) — the per-ITEM half of the shell check over a caller-supplied merge (declared env vars minus `PATH`, plus aliases, against the primary managed env file); a missing file is per-item absence, an unreadable one the `check_error`. The whole-file, rc-line and `PATH` checks stay machine-wide in `env_verify_results`; `diff --module`'s Shell section is the scoped consumer.
- `reconciler::primary_env_file(home)` — the primary managed env file on this host, and the ONE public spelling of that platform split.
- `FoldedPath` + `fold_path_line(...)` / `primary_folded_path(...)` (`reconciler/env_engine.rs`) — the ONE `PATH` line a generated env file carries, over both producers and both readers; a dialect renders the parts through `FoldedPath::value(quote, inherited, separator)` and never decides which entries or in what order.
- `module_status_display(stored, drifted)` + `MODULE_STATUS_INSTALLED` / `MODULE_STATUS_ERROR` (`state/types.rs`) — the ONE derivation of the word a person reads for a module's state. `Drifted` is DERIVED, so a recorded-state surface passes the RECORDED drift verdict — whether the same report renders an unresolved finding for the owner — never a literal `false`.
- `ENV_SESSION_RESOURCE_ID` (`state/types.rs`) — the recorded `resource_id` of the live-session env surface; never the bare `"refresh"` literal.
- `Action::pre_skip_reason()` (`reconciler/types.rs`) — why an action cannot run on THIS host, answered while the plan is read. The ONE seam: `Phase::action_count` excludes what it names and `render_plan_tree` settles it, so the plan's promise and the apply's tally are one number.
- `reconciler::attempted_count(actions)` (`reconciler/types.rs`) — the ONE spelling of that exclusion as a COUNT, over any action iterator; never hand-roll the filter, and never price a `--phase` payload off the unfiltered plan.
- `ActionResult.not_attempted` + `ApplyResult::not_attempted()` + `RunTally.not_attempted` + `ApplySummary::Actions.not_attempted` — the SAME predicate on the apply side: a withheld action is neither `succeeded` nor `skipped`, so `succeeded + skipped + failed == planned_total` holds. `outcome_clauses(tally)` (`reconciler/run.rs`) is the ONE decomposition of a run's outcomes — one clause per class, one clause per LINE; `outcome_counts` joins them for the daemon's log line. `every_outcome_class_in_a_rollup_carries_its_own_role` walks the classes.
- `output::Elapsed` (`::row(d)` / `::wall(d)`) + `StatusBuilder::wall_duration(d)` — what a duration slot MEASURES beside how long: `.duration(d)` is a row's own span (` (23.8s)`), `wall_duration` the closing rollup's wall-clock total (` (278.2s wall)`), and `render_run_rollup` its one caller. The ROW slot times what RAN, failures included — `apply::failed_action_ran` is the ONE exclusion for failures that ran nothing. The wall total hangs off the rollup's FIRST line, and every rollup line reserves the glyph column.
- `SystemContext::next_step(message)` / `NoteSink::next_step(printer, message)` (`providers/mod.rs`) — an INSTRUCTION from a configurator, routed as a hint; `report(role, …)` is for what the run says about itself (`Warn` iff the reader must act, never `Ok`). `every_provider_note_takes_its_role_from_whether_the_reader_must_act` walks the population.
- `packages::shared::brew_caveat_asks_the_reader_to_act(body)` — whether a brew `==> Caveats` body is an instruction (`Warn`) or a report (`Info`); brew only. The note's SUBJECT is settled one layer up by `ActionNote::attributed_to(subject)` in `collect_caveats` — one slot, never a second tag vocabulary.
- `backup::outcome_role(clean, produced_artifact)` + `outcome_detail(error, size)` + `snapshot_subject(destination)` / `restore_subject(target, snapshot)` / `rollback_subject(target, copy)` (`backup/mod.rs`) — the three-way role, the one detail slot and the three SUBJECTS every completed backup-engine outcome settles as; the three report verbs read all of them, so an outcome cannot be worded twice.
- `backup::safety_copy_hint(safety, name)` (`backup/mod.rs`) — the ONE closing hint a restore or rollback leaves after displacing live data; both readers reach it, never a second literal.
- **The grammar split** (`output-module.md`, "The grammar split, stated once") — a run BODY row is lowercase imperative, a RESULT line is sentence case past tense, a provider NOTE is a sentence or the command as run. `every_action_row_subject_opens_on_a_lowercase_verb` and `every_result_line_is_sentence_case` PARTITION the sources; `// name-row-ok: <why>` hatches a row that names rather than reports.
- `reconciler::action_produced_detail(action, installed, delivered, versions)` + its four `*_summary` arms (`reconciler/apply.rs`) — the fact an action PRODUCES, worded for its own row's detail slot; the ONE producer read by the plan bullet, the apply row and `PlanActionOutput.detail`. A detail never restates a total the subject already gives; a shortfall states the COMPLEMENT; provenance clauses come from `delivered_by_this_run`. A new produced count adds an arm here, never a `format!` at a call site (`every_produced_count_is_an_action_rows_detail`).
- `reconciler::link_deployed_digest(source)` + `Reconciler::refresh_link_deployed_hashes(fm, resolved, modules)` (`reconciler/files.rs`) — the ONE digest of what a converged link entry DEPLOYS (directory links folded file-by-file, symlinks skipped as the deploy skips them), read by both halves of the recorded-hash refresh; `None` on any unreadable file abandons the whole digest.
- `ApplyStatus::{as_str, display_str, human_str, human_display}` (`state/types.rs`) — the stored / `-o json` / human spellings of an apply's outcome, pinned as a wire contract; `human_display` pairs the human word with its role, and every slot rendering the word (`status`'s Result row, `log`'s Status column) takes the pair.
- `ApplySummary` + `::to_column()` / `::prose(stored)` (`state/types.rs`) — the ONE typed shape of the `applies.summary` column and the ONE prose rendering every human surface reads it back through. A slot names its outcome only when it OCCURRED (`no_summary_slot_names_an_outcome_that_did_not_occur`); never build the column with `json!`.
- `cli::apply::refresh_link_deployed_hashes(reconciler, resolved, modules)` (`crates/cfgd/src/cli/apply.rs`) + `Reconciler::file_manager()` — the ONE post-apply settle of the hashes an apply just recorded: a verb that records rows also settles their content hashes before it returns, or the next daemon tick reports work the machine never did. `every_plan_running_verb_settles_its_link_deployed_hashes` walks the population (`// no-hash-refresh-ok:` for a preview).
- `ApplyResult::{succeeded, skipped, failed}` + `RunTally.skipped` (`reconciler/`) — a successful action that CHANGED nothing is skipped, not done. Every count comes from these; `!failed` is not a success count.
- `ManagerAction::Provision { declared, .. }` + `DeclaredProvision` (`reconciler/types.rs`, minted by `managers::declared_manager_routes`) — the module's own route to a tool cfgd bootstraps as a manager; minted only when the AUTHOR named the manager (`ResolvedPackage::manager_declared`). With no route the cascade prices against the managers this run will DELIVER, and `Reconciler::elide_provisioned_tools` keeps one run from planning a package twice; the full route/elision/membership reasoning is on those items' rustdoc, pinned end to end by `a_tool_this_plan_provisions_is_not_planned_again_by_a_module_entry` and siblings.
- `PackageExec::installed_now(pm, cx)` (`reconciler/packages.rs`) + `Reconciler::package_survives_elision` (`reconciler/plan.rs`) — the EXECUTE-time re-read of what a manager has, and the ONE predicate every elision judges by; an install action re-reads before it runs, installs only what is still missing, and settles `changed: false` when nothing is left. Fail-OPEN on an unqueryable manager. Verification failures word themselves from what the run DID (`every_bootstrap_failure_names_what_it_installed`).
- `PackageExec::refuse_withheld_manager(name)` + `usable_manager(name)` + `withholding_managers(list)` + `Reconciler::unprovisioned` + `ManagerAction::managers_left_unavailable()` — a manager a node of THIS run failed to provision is unavailable for the rest of it, answered from the failed node and never re-probed (`is_available()` cannot be trusted after the memo moved). `a_package_action_for_a_manager_whose_provision_failed_is_never_spawned` pins both shapes.
- `PackageSchemaPath::noun()` + `DEFAULT_PACKAGE_NOUN` (`config/resolve.rs`), reached from the CLI through `PackageRef::noun[_capitalized]` — the word a confirmation line calls a `--package` entry, read off the schema path so add and remove cannot disagree.
- `ActionNote::next_step(message)` (`providers/mod.rs`) — a caveat that is an INSTRUCTION rather than a report; renders as a hint after every report in its group.
- `note_tag_doubling_error(tag, message)` (`providers/mod.rs`) — the ONE statement of the rule that a note's body never opens on its own tag; the twin of `system_key_doubling_error`.
- `reconciler::render_caveats(printer, groups)` (`reconciler/apply.rs`) — the ONE render of the closing `Caveats` section, over both callers. A caveat is deduplicated by MESSAGE across the whole report (render-only; `-o json` keeps every note under its owner). `every_caveat_slot_dedupes_by_message` pins it.
- `cli::status::recorded_module_tallies(items, declared)` → `ModuleTally` — what the Managed Resources table says this host manages per module, and the ONE source of the Component Health module rows' counts. `every_module_owned_kind_the_table_lists_has_a_slot_in_the_headline` walks `display_type`'s arms.
- **A Component Health row counts what the table lists, and a zero clause DROPS** — `(1 file)`, never `(0 packages, 1 file)`; an all-zero owner reads its bare verdict. `the_component_health_counts_what_the_table_lists` pins the non-module agreement beside the module pin.
- **Aliases lead env vars on every surface naming the pair** — the two halves of the shell surface carry no header saying which is which, so every surface orders them one way. `every_surface_naming_an_env_block_names_its_aliases_first` walks the CLI sources; `every_surface_naming_the_shell_pair_lists_aliases_first` renders each member.
- **`status <module>`'s header leads on its two themed rows** — `Status`, then `Scope`, ahead of the recorded ages and counts; `the_status_and_scope_rows_lead_a_module_report` pins the order.
- `cli::status::managed_resource_rows(items, modules, profile, detail)` + `owner_render_order(rows)` (`crates/cfgd/src/cli/status.rs`) — the Managed Resources rows and the ONE place its Owner column is decided and ordered: the vocabulary is `reconciler::owner_of`'s, the order `Owner::order`. Wide = FULL granularity for the fleet table: `ManagedResourceDetail` carries `wide`, the module manifests and the resolved strategies, blowing a `files:<n>` aggregate into one row per file with its Method; a manifest of ONE renders the bare folded path on every table. Display only; `-o json` carries the raw rows. `the_fleet_wide_table_lists_one_row_per_deployed_file_with_its_method` pins it.
- `reconciler::recorded_env_method(resource_id)` + `ENV_VERB_WRITE`/`ENV_VERB_INJECT` (`reconciler/env_engine.rs`) — the `write`/`inject` verb a recorded env row was produced by, reconstructed from the target's basename because the recorded id drops it; the two consts are the ONE spelling every verb producer reads. Welded to `env_targets` by `every_env_target_classifies_under_the_verb_that_produced_it`.
- `FileStrategy::from_recorded(s)` (`config/profile_spec.rs`) — the inverse of `as_str` for reading `module_file_manifest.strategy` back; `None` for a value no variant spells, so a corrupt column degrades to an absent cell.
- **`FileStrategy::default()` is not the configured default** — the plan settles `spec.fileStrategy` into every `DeployFiles` action, so a reader never resolves a strategy-less entry itself; the only correct fallbacks are `registry.default_file_strategy` or `FileStrategies`. An `unwrap_or_default()` on a `FileStrategy` outside that settle is a bug.
- `StateStore::prune_module_files_except(module, declared)` (`state/modules.rs`) — drops manifest rows for files the module no longer declares; the deploy path calls it so the manifest mirrors the LAST-APPLIED declared set and agrees with the `files:<n>` aggregate recorded beside it.
- `FileStrategies::for_declared_target(target)` (`effective.rs`) — the resolved strategy of a still-DECLARED target only; `None` for a recorded row outliving its declaration, which renders `ABSENT` rather than a guessed default. `for_target` stays the blanket-default read.
- `Owner::TOKEN_SEPARATOR` (`reconciler/types.rs`) — what joins several owner tokens into one string (an isolated run's recorded scope); the writer and `cli::status::scope_row` must agree with it.
- `Owner::label()` / `Owner::token()` (`reconciler/types.rs`) — the ONE composition of an owner's `kind:name`, styled and plain; every surface holding an `Owner` asks it. `OwnerLabel::new(kind_literal, name)` stays the composer for a call site holding no `Owner`. `every_owner_label_of_a_held_owner_comes_from_owner_label` walks both crates.
- **A row's VERDICT leads its detail; its counts are the parenthetical** — `✓ module:nvim — Synced (24 packages, 6 files, 7 scripts)`: the health word a reader scans for never lands behind the inventory. The word renders through `StatusFields::verdict`'s renderer-owned role-styled slot (`output-module.md`). `no_status_detail_trails_a_verdict_word_behind_its_counts` walks the population.
- `ApplyRun::unplanned(ctx, actions)` + `RunContext::subject` / `RunContext::unit_source` (`reconciler/run.rs`) and `backup::RESTORE_ACTION_COUNT` — the plan-less run: a command whose body it renders itself still takes the skeleton's header and rollup. Never a synthesized empty `Plan`, and never a second header renderer.
- `reconciler::pseudo_phase(printer, label)` / `sole_phase(printer)` (`reconciler/run.rs`) — the two ways work outside a `Plan` opens its owner groups: under a `Phase:` row beside other phases, or at the run's own depth when the run has no other phase. Never a `Phase:` row that only restates the run's title.
- `RunTitle` + `::as_str()` (`reconciler/run.rs`) — what a run calls itself: the heading AND the rollup's noun. A new run kind adds a variant here, never a literal.
- `backup::report_restore(printer, outcome)` — the ONE render of a restore's rows and rollup, returning the `RunTally` its caller reports.
- `reconciler::report_align_width(plan, filter, budget)` + `report_trailing_allowance` + `Printer::report_column[_beside]` / `live_column_for` — ONE alignment column per REPORT, claimed only if the widest trailing a row can print beside it still fits; the claim, allowance and subject budget are one computation (`report_subject_budget`), priced over each subject's FIRST physical row (an operand list wraps, never cuts). A live painter pads to the same claim; the one cut of cfgd-authored text retreats to a token (`clamp_at_token`). The pricing story is on those items' rustdoc; `the_reports_column_is_claimed_and_its_subjects_bounded_at_every_width` walks 80..=200.
- `output::renderer::{action_subject_style, action_detail_is_muted}` — the ONE emphasis mapping for an action row's two halves, read by all three painters: a WITHHELD role mutes the subject and brightens the reason. Never decide either half at a call site.
- `reconciler::lanes::wait_reason(Hold)` + `lanes::Hold::{Edge, Source, Lane}` + `live_tree::Wait { subject, reason }` — a blocked row's two SLOTS and the ONE wait sentence per hold kind: `waiting on <row>` for a dependency (edge or hoisted source registration), `queued behind <row>` only for an unrelated lane turn. The reason fills the DETAIL slot; `every_hold_kind_carries_its_own_wording` binds every variant.
- **Owner-carrying composers** (`output/`) — `Printer::heading_owner_prefixed`, `section_owner[_or_collapse]`, `SectionGuard`'s pair, `Doc::section_owner` / `subsection_owner`. Reach for the one matching the call site's shape; cataloged in `output-module.md`.
- `Printer::command_list(pairs)` and its `SectionGuard` / `Doc` / `SectionBuilder` counterparts — a "command — description" list, for a left column that NAMES a thing; never for an ordinary key/value fact. Cataloged in `output-module.md`.
- `CommandPair::typed(key, type_span, value)` / `KvPair::{annotated, nested, role_valued, owner_valued}` / `TitleLabel::typed` (`output/component.rs`) — the renderer-owned styling and layout slots; a caller never paints or indents one itself. `owner_valued(key, owners)` is the ONE kv slot an owner token may occupy; `cli::status::scope_row` is its one caller.
- `KvPair::linked(key, text, url)` (`output/component.rs`) + `output::terminal_supports_hyperlinks()` + `config::docs_url(path, version)` (`config/modeline.rs`) — a kv value that is a LINK, its capability gate and the release-pinned URL derivation; the renderer prints the short text only where the theme carries hyperlinks, the URL itself everywhere else. `docs_row(path, url)` (`cli/explain/mod.rs`) composes the `Docs` row; `every_docs_pointer_the_cli_renders_goes_through_the_linked_slot` walks for hand-built ones.
- `Doc::paragraph(text)` (`output/doc.rs`) — a prose paragraph for what a documentation surface says ABOUT the heading above it; cataloged in `output-module.md`.
- `AccentHeading` (`output/accent_heading.rs`, `pub(super)`) — the ONE composer for the "Caveats" heading; deliberately not a `PhaseLabel`.
- `Printer::narrate(running, |sp| work)` / `Printer::narrate_silent(...)` — the settle-safe spinner wrappers; never hand-roll guard + spinner + finish. Which one a wait takes is decided by who else SAYS the failure. Cataloged in `output-module.md`.
- `pluralize(count, noun)` / `plural_noun(count, noun)` / `agreeing_verb(count, verb)` — the ONE agreement rendering for a counted sentence, regular English only. The noun names the unit the count is IN — a count of what MOVED is never the coverage of the record that moved, and a completion clause describes the MACHINE, never cfgd's bookkeeping. `every_counted_clause_names_the_unit_it_counts` walks the daemon's clauses.
- `sentence_case(word)` — capitalize the first character and leave the rest alone; never a title-caser.
- `yes_no(Option<bool>)` — the ONE `yes` / `no` / `-` rendering of a tri-state fact in a table cell. `None` reads "not known", never "no".
- `ABSENT` (`util/strings.rs`) — the ONE token a table cell renders for a fact nothing recorded (`-`); one spelling is what lets `Table::without_unfillable_columns` judge a column. A PAST instant's absence reads `never` and is a fact, not an absence.
- `Table::without_unfillable_columns()` (`output/renderer/table.rs`) — drop every column whose every cell is `ABSENT`; every `Table` the CLI emits settles through it (`every_listing_the_cli_renders_drops_a_column_no_row_can_fill`).
- `last_sync_display(last_fetched, now)` + `SOURCES_SECTION` (`crates/cfgd/src/cli/source/list.rs`) — the ONE human rendering of a config source's last fetch and the ONE noun for the section listing sources; `-o json` keeps the ISO 8601 instant.
- `sources_table(entries, wide, now)` + `configured_source_entries(cfg, state)` (same file) — the ONE `Sources` table and the ONE derivation of the declared catalog it renders; a surface holding live facts merges them OVER a catalog row. Every `SourceListEntry` slot but `name` and `status` is an `Option`, and the builder drops a column no row can fill.
- `daemon::SourceStatus.drift_count` + `DaemonStatusResponse.drift_count` — outstanding drift is a machine-wide HEADER fact (`current_drift_count(store)`), never a per-source cell; a write targeting one source finds it BY NAME. `no_daemon_state_write_reaches_a_source_row_by_position` walks the daemon (`// positional-source-ok:` hatch).
- `daemon::reconcile_tick`'s outcome sentence (`daemon/reconcile.rs`) — the ONE line a tick leaves on the journal; every count-returning arm folds into it at one seam (`every_error_only_arm_of_the_reconcile_tick_is_classified` holds the table).
- `output::config_header_rows(config_path, sources, profile, modules)` (`output/component.rs`) — the ONE header block every surface reporting on a resolved configuration opens with: `Config`, `Sources`, `Profile`, `Modules`, in that order, ahead of every surface-specific row. Slots are `Option`/slice; `ComposedSource::from_declared(specs)` derives the `Sources` value from the DECLARED list; the four facts travel as `output::ConfigHeader`. `every_config_and_profile_header_row_comes_from_the_one_builder` walks for hand-built rows (`// header-row-ok:` for a row naming a different fact).
- `output::modules_header_row(names, skips)` + `HeaderModule::of_resolved(modules)` + `modules_header_row_for(modules)` (`output/component.rs`) — the Modules-only primitive the header wraps, naming the RESOLVED list (transitive `depends`, dependency order), never `merged.modules`; `skips` annotates platform-gated modules. `Sources` and `Modules` travel together on every run naming a profile (`every_run_under_a_resolved_profile_names_its_sources_and_modules`, with per-slot hatches); the per-surface derivation story lives on the builders' rustdoc.
- `daemon::SourceStatus.last_commit` — the commit a source's checkout is at, seeded at daemon start and moved by every accepted pull; `daemon_source_row` prefers it over the catalog's recorded commit.
- `source_failure_next_step(err, name)` + `subscription_knob_label(key)` (`crates/cfgd/src/cli/source/mod.rs`) — what a reader DOES about a refused source, per error kind, and the rendered label for a subscription knob. Display-only.
- `daemon::PullOutcome` + `PullFailure[Kind]` + `git_pull_sync` / `is_git_repository` / `pull_failure_summary` (`cfgd-core/src/daemon/git.rs`) + `cli::local_pull_next_step` + `MSG_NOT_A_REPOSITORY` (`crates/cfgd/src/cli/mod.rs`) + `sync::sync_refused` (`crates/cfgd/src/cli/sync.rs`) — the ONE classification of a local pull, read by all three pulling callers, so none can call an unversioned directory a failure (exit 0) nor disagree on a refusal's exit code. `local_pull_next_step` matches the kind with no wildcard; `sync_refused` is the ONE predicate for `cfgd sync`'s exit (a declined prompt is an answered decision). `a_local_pull_failure_exits_1_from_both_verbs_that_pull` pins the agreement.
- `cli::output_types::SourceOutcome` + `::refused()` / `::declined()` — the ONE vocabulary for the per-source state `sync` and `source update` put on the wire, and the ONE reading of which states are refusals.
- `SourceManager::set_announce_cache_skips(bool)` (`sources/mod.rs`) + `RunContext::fetching_sources()` / `announce_cache_skips()` — whether a cache-freshness skip restates itself: the command that IS the fetch holds the two `cfgd sync` advisories back, while disclosures (allowScripts) still print. Never a blanket Quiet over the resolution.
- `success_next_step(Mutation)` (`crates/cfgd/src/cli/mod.rs`) — the ONE closing hint every MUTATING verb ends on, across all five families (`source`, `module`, `profile`, `secret`, `rollback`): a trust edit points at `cfgd sync`, a composition edit at `MSG_RUN_APPLY`, an artifact at its consumer, `rollback` at `cfgd diff`. A hint never re-spells the verb that ran with concrete arguments, never names a file the verb consumes, and every backticked span is complete as printed. `every_mutating_verb_closes_on_a_next_step` walks the families.
- `reconciler::run_next_step(tally, title)` (`reconciler/run.rs`) — the ONE next step every non-`Success` verdict closes on, worded per state with a PLACEHOLDER command off `RunTitle`; a verdict line states facts only. `every_unfinished_verdict_closes_on_the_one_next_step` walks every status × title.
- `heal_drift_hint(module)` + `perform_preview_hint(&PreviewScope)` + `MSG_RUN_APPLY` (`crates/cfgd/src/cli/mod.rs`) — the THREE next-step wordings a non-mutating surface closes on, split by what the reader has just seen (unseen changes / found drift / the preview itself). `PreviewScope` re-renders the preview's own flags so the composed command re-parses. `every_verdict_that_shows_pending_work_names_the_command_that_settles_it` walks the states.
- `answer_decisions_hint(pending)` (`reconciler/pending.rs`) — `MSG_ANSWER_DECISIONS` with the bulk form folded in; rendered by exactly two composers, each closing the decisions section from inside.
- `build_pending_decisions_table_section(section, decisions, contents)` (`crates/cfgd/src/cli/source/helpers.rs`) — the ONE buffered render of a decisions section (`decide`, `status`).
- `head_signature_accepted(name, repo_dir)` (`sources/mod.rs`) — whether a source's checked-out HEAD carries a signature cfgd would accept, through `verify_head_signature`'s own classifier. `None` is "cannot say", rendered `-`.
- `action_display_subject(action)` (`reconciler/format.rs`) — the ONE display derivation of an action's subject; a preview, an alignment column and an executed line must be one string. An operand-list subject (a package install) names EVERY operand at every width (a long list wraps, never cuts); `DeployFiles` is the one exception, whose subject is a bare count and never its targets — see `deploy_file_children` below. Never persist a `DisplaySubject`. `lanes::node_subject` / `lane_occupant` / `blocker_subject` narrow it for wait lines: a blocker is named by the row the reader can see, never by a manager token or an operand list. `every_manager_action_subject_names_every_operand_it_holds` binds the variants.
- `deploy_file_children(action)` (`reconciler/format.rs`) — every file a `DeployFiles` action writes, target then resolved `FileStrategy::method_label()`, in manifest order; the ONE producer `render_plan_tree` and `apply::emit_action_line` both drain as the action's child rows, so a preview and its settled row cannot enumerate two different lists. `None` for every other action kind.
- `SectionGuard::child_row(target, method)` (`output/section_guard.rs`) — the ONE render of a deploy row's per-file child (`<target> — <method>`, no glyph, two depths below its parent); a caller never paints or indents one by hand. `Emitting::child_row_column(depth)` (`output/renderer/status.rs`) is the column it pads to, and `report_align_width` is what prices every child's effective width into the claim that column reads.
- `script_run_subject` / `module_script_subject` / `hook_script_subject` / `bare_script_subject` (same file) — the partial views for callers holding a script's parts; never rebuild `"{marker}: {body}"` by hand.
- `condense_action_desc_for_display(action, desc)` (same file) — the narrower gate for a raw description string that is not an action subject; never apply it to a value you persist.
- `compose_in_flight_subject(theme, text)` (`output/spinner.rs`) — the ONE composition of an in-flight label, over all five live entry points: it FOLDS and paints the text, never edits it — a trailing `…` is part of the subject. `no_in_flight_label_carries_a_trailing_ellipsis` walks call sites for decorative ones.
- `system_resource_key(configurator, key)` (same file) — the ONE composition of a system setting's `<configurator>.<key>` identity; three surfaces mint and match it.
- `system_key_doubling_error(configurator, key)` (same file) — the ONE statement of the no-self-prefix rule and its diagnostic.
- `pre_skip_doubling_error(subject, reason)` (same file) — the third member of that family, for a withheld row's two slots: a subject never repeats the noun its reason opens on (`no_pre_skip_reason_repeats_a_noun_its_subject_already_names` holds the table).
- `compliance::snapshot_content_hash(snapshot)` — the ONE serialize-and-digest for a compliance snapshot, dropping the volatile timestamp.
- `CFGD_BACKUP_SUFFIX` / `cfgd_backup_path(target, extra)` / `backup_file(target)` (`reconciler/sidecar.rs`) — the ONE spelling and writer of the sidecar cfgd leaves beside a target it displaces, with `SidecarOutcome::detail()` the ONE wording both readers render. One stamped copy is retained per target, pruned where the sidecar is WRITTEN; the primary `<target>.cfgd-backup` is never pruned, and `backup::rollback_copy(unit)` reads the newest survivor. `every_sidecar_report_is_worded_by_sidecar_outcome_detail` walks for call-site verbs.
- `Reconciler::backing_up(targets)` — the targets a conflict settled as `Backup`, copied aside as the displacing action executes; both file-writing paths route through `back_up_adopted_target`.
- `is_unmanaged_file` / `sweep_unmanaged_file_targets` / `apply_conflict_policy` / `sweep_label` / `ResolvedConflict` / `UNMANAGED_SKIP_REASON` / `unmanaged_conflict_error` (`reconciler/adopt.rs`) — the ONE classification of "does this target hold a file cfgd never wrote", and the ONE non-prompting sweep. The CLI keeps only the PROMPT.
- `mark_unmanaged_drift(record, strategy, config_dir, state)` + `UNMANAGED_DRIFT_CAUSE` (same file) + `FileDriftResult.unmanaged` — the READ-side half: a drifted finding on a target cfgd never wrote is a different problem with a different fix. All four producers mark.
- `effective::effective_file_strategies(profile, modules, config_dir, default)` — where a producer holding only a target looks its RESOLVED strategy up; per-caller `unwrap_or(default)` is how two surfaces disagreed.
- `PushOutcome` / `PackOutcome` + `oci::artifact_row_detail(digest, platform)` (`cfgd-core/src/oci/`) — what a push returns and how its settled row words it: digest plus the RESOLVED platform, stated unconditionally; a caller never re-derives the platform or serializes the `--platform` flag in its place. `no_artifact_verb_serializes_its_platform_flag_as_the_platform_it_resolved` walks the three artifact verbs.
- `oci::artifact_platforms(reference)` (`cfgd-core/src/oci/pull.rs`) — the platforms an already-pushed artifact declares, off its manifest alone; reached through `ArtifactPlatformReader`, never directly.
- `cli::status::drift_checked_note(checked_live, last_scan_at, now)` + `freshest_check_stamp(last_scan_at, row_stamps)` — the ONE human rendering of a drift check's freshness in all three states (`checked live now` / `checked 3m ago` / `drift never checked`) and the ONE fold picking the instant it dates: the freshest of the machine-wide stamp and the recorded rows' own timestamps, feeding the fleet Component Health annotation — where the fleet's empty drift verdict LIVES — the module Drift qualifier and both surfaces' scan-hint staleness gate; the age degrades through `humanize_age_cell` like every other age slot.
- `status::drift_section(doc, drift, check_errors, checked_live, verified, scan_note, row)` (`crates/cfgd/src/cli/status.rs`) — the ONE frame of the MODULE surface's Drift section and the ONE place its empty role is chosen: `Ok` only when a check stands behind the verdict. The fleet surface has no Drift section — each finding nests under its owner's Component Health row (`component_health_rows`' one walk). `no_recorded_verdict_claims_a_check_that_never_ran` pins it beside `diff`'s refusal.
- `SIGNATURE_VERIFIED` / `SIGNATURE_UNVERIFIED` / `SIGNATURE_UNSIGNED` / `SIGNATURE_UNKNOWN` (`cfgd-crd/src/lib.rs`) — the ONE four-word vocabulary for a module's signature, written only by the controller that ran the check; absence reads `unknown`, and no negative word is derived from `status.verified` plus the spec.
- `oci::check_signature(reference, opts)` → `SignatureCheck` (`Valid` / `Rejected` / `Undetermined`) (`cfgd-core/src/oci/sign/mod.rs`) — `verify_signature` as a three-way verdict for every caller that displays or records the outcome; a bare `Err` is not "unverified".
- `OciReference::uses_plain_http()` (`cfgd-core/src/oci/mod.rs`) — whether cfgd reaches this registry over HTTP; `api_base` and cosign both read it, and every cosign subcommand declares the scheme (`every_cosign_subcommand_this_module_spells_declares_the_registry_scheme`).
- `ArtifactFactsReader` + `ArtifactVerifier` + `RegistryBackoff` (`cfgd-operator/src/controllers/mod.rs`) — the two seams a Module reconcile reaches a registry through, and the memo that keeps a failed visit from repeating. Every registry-reading component carries the same chart knob (`every_registry_reading_component_exposes_the_same_registry_knob`).
- `Reconciler::recording_scope(scope)` — what a `--module` run's `applies` row records in place of the profile name; `cli::status::derivable_profile` is the ONE read of it.
- `explain::LevelWidths::of(fields)` + `field_row` / `push_tree_rows` (`crates/cfgd/src/cli/explain/mod.rs`) — the three column widths a field level's rows pad to, so the `[+]` mark and ` (required)` land in columns a reader can scan. `every_field_row_mark_lands_in_a_column` walks both marks on both surfaces.
- **A union-typed field in `explain` renders its shapes once and its fields by name** (`schema::union_type_join` / `union_variants`, `explain::own_fields` / `row_type_span` / `is_expandable`, `FieldNode::is_variant`) — a `$ref` member renders its `$defs` name, a Variants row carries one fact, a union's one object arm lists its fields where a plain object would, and a variant row never earns the `[+]` mark. `every_union_typed_field_renders_its_shapes_once_and_its_fields_by_name` walks every union field.
- **No `explain` description names a POSITION** (`crates/cfgd/src/cli/explain/mod.rs`) — the renderer sorts every level alphabetically, so a doc comment names its SIBLINGS, never "below"/"above". `no_explain_description_names_a_position_on_the_screen` walks every field of every kind.

## Error wording

- `CfgdError`'s variants (`errors/mod.rs`) — a top-level variant carries a `<noun> error:` label only when the label is LOAD-BEARING for some member of its inner enum; a sentence that names its own subject reads without one, and `CfgdError::kind()` is what `-o json` routes on. The verdict is a judgment held in `no_error_a_row_renders_opens_on_a_category_label`'s table, which checks the source against it so a new variant is classified before it ships.
- `PackageError::BootstrapFailed` — the one variant whose sentence its CALLERS build (`#[error("{message}")]`); every construction site names its own manager or package.

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
- `fold_home_in_text(text)` — the ONE DISPLAY inverse of `expand_tilde`: every `<home>/` in a display slot reads `~/`, so one report cannot spell `$HOME` two ways. The population is every DISPLAY slot — subjects, header kv values, cells/rows rendering recorded paths, every closing hint (folded once at `Renderer::render_hint`), every confirm-prompt question — never a stored id, `-o json`, a `tracing!` line or a human-facing error (`path-handling.md`). `no_report_slot_spells_the_home_directory_absolutely` and its three sibling walks pin the population from both sides.
- `normalize_path_entry(entry, home)` — fold ONE `PATH` entry to the form two spellings of the same directory compare equal in. COMPARISON only; never render the result.
- `PATH_LIST_SEPARATOR` / `is_inherited_path_ref(segment)` — the host's `PATH` separator and the ONE predicate for a segment that REFERS to the ambient `PATH`; read by `fold_env_layer` and the env engine's own `PATH` line.
- `absolutize_path(path)` — make a path absolute LEXICALLY without requiring it to exist; use at any CLI entry point. Never canonicalizes, so a symlinked config keeps the name the user gave it.
- `resolve_relative_path(path, base)` — resolve relative to base with traversal validation.
- `resolve_managed_file_source(source, config_dir)` — the ONE resolution of a `spec.files[].source` against the config dir, taken by BOTH readers of that field.
- `validate_path_within(path, root)` — canonicalize and verify containment.
- `validate_no_traversal(path)` — reject a reference containing `..` or naming nothing of its own; use for any path cfgd reads or writes.
- `validate_plain_name(raw)` — stricter, judged on the RAW string, for any string that NAMES something cfgd creates under a root it may later delete or mount wholesale; Windows shapes are rejected on every host.
- `atomic_write(target, content)` / `atomic_write_str` — atomic temp+rename write returning the SHA256; use instead of `fs::write` in ALL production code. Replaces a symlink at the target rather than following it.
- `atomic_write_merged(target, content)` — the `strategy: Patch` write: resolve a symlink first, so the target keeps its mode and its link identity.
- `atomic_write_resolved[_str](target, content)` — the FOLLOW-the-symlink variants, for a user-owned file where a stow/chezmoi link must survive. A dangling link is written at the link path itself.
- `ensure_parent_dir(target)` — create a file's parent; use instead of the inline `if let Some(parent)` idiom.
- `write_scaffold(kind, path, body)` (`crates/cfgd/src/cli/helpers.rs`) — scaffold writes in the binary crate: the modeline pinned to the BINARY's version, plus an atomic write. Rewrites of user-owned files must not use it.
- `rewrite_user_yaml(path, &value)` (same file) — rewrites of user-owned YAML: re-prepends the leading comment block, prunes absent sections and undeclared scalar defaults. Use instead of raw `to_string` + `atomic_write_str`.
- `quoted_assignment(name, value)` (same file) — the ONE rendering of a declared env var or alias as `name="value"`, quoted through the same quoter the generated env file is written with, so the confirmation and the file spell one assignment one way. NOT for a pre-approval review surface, which must show declared bytes. `every_assignment_a_setter_confirms_renders_through_the_one_quoter` walks the population.
- `copy_dir_recursive(src, dst)` — recursive tree copy; correct ONLY where cfgd owns the destination.
- `carry_dir_mode(src, dst)` — best-effort directory mode copy; call it AFTER populating `dst`.
- `create_symlink(source, target)` — cross-platform; Windows picks the reparse type by resolving a relative target against the LINK's own parent, and refuses a privilege-less host through `symlink_error` — the ONE Developer Mode wording, which every preserving copy wraps rather than degrading to a silent skip.
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
- `modules::resolve_package(entry, module, platform, managers, installed)` — the ONE site that decides which manager a module package lands on, and the ONE producer of a `ResolvedPackage`: a bare entry is satisfied by the available manager that ALREADY HOLDS it, an authored `prefer` is honoured over a holder, and only a package nobody holds falls to the platform default. Every planning path threads its context. `every_resolved_package_producer_routes_through_the_one_resolver` walks for a second producer.
- `modules::fill_available_versions(packages, managers)` — the unconditional per-package form for the two surfaces that print a version without planning (`doctor`, `module show`); no planning path may call it.
- `command_output_with_timeout(cmd, timeout)` — run a `Command` with a timeout, killing on overrun. It OWNS the stdio configuration; never set stdio yourself.
- `terminate_process(pid)` — SIGTERM / TerminateProcess.
- `exit_status_reason(status)` — the ONE rendering of why a child ended; never `status.code().unwrap_or(-1)`.
- `stdout_lossy_trimmed(output)` / `stderr_lossy_trimmed(output)` — trimmed lossy-UTF8 capture.
- `output::renderer::wrap::wrap_body(body, prefix, cols)` / `wrap_body_with_trailer(...)` (`output/renderer/wrap.rs`) — the ONE layout of a message body into physical rows: every continuation hangs under the first word after the glyph, and a logical line's OWN indent stacks on that hang. `a_wrapped_line_keeps_its_own_indent_on_its_continuation_rows` pins it.
- `output::captured_output_detail(msg)` — the ONE fold from a child's captured output into a rendered DETAIL slot: bounded at the live window's `VISIBLE_LINES`, elided from the MIDDLE. `collapse_to_subject_line` is its one-physical-row twin. Both DISPLAY-only; a manager's stderr reaches either only through `packages::shared::command_failure_reason`.
- `packages::shared::hand_child_bootstrapped_path(cmd)` + `run_pkg_query(manager, cmd)` / `pkg_run` / `run_pkg_cmd*` (`crates/cfgd/src/packages/shared/mod.rs`) — the ONE handoff of the directories this run bootstrapped to a manager child, and the wrappers every manager spawn under `packages/` takes (`run_pkg_query` also bounds the wait). A non-manager spawn carries `// own-path-ok: <why>`. `every_manager_spawn_under_packages_inherits_the_bootstrapped_dirs` walks the crate.
- `is_root()` — euid==0 / `IsUserAnAdmin()`.
- `hostname_string()` — system hostname; `"unknown"` on failure.
- `tracing_env_filter(default)` — `EnvFilter` from the environment with a fallback.
- `env_or(var, default)` — read an env var with a fallback; the ONE spelling for the two server binaries.
- `await_shutdown_request()` — the ONE SIGINT+SIGTERM registration-and-select for a server binary (`#[cfg(unix)]`); a caller adds logging and never its own handler. `daemon::ShutdownSignals` is deliberately separate.
- `output::claim_termination_signals()` — tells the live region's cursor-restore hook that THIS process handles SIGINT/SIGTERM itself; called first by every cooperative registration.
- `require_tool(name, install_hint)` — the uniform "X not found" error for every `command_available`-gated flow.
- `tool_cmd(env_var, default)` — the generic seam-honouring `Command` factory.
- `systemctl_cmd()` / `systemctl_available()` / `SYSTEMCTL_BIN_ENV` — the ONE `systemctl` factory, predicate and seam. Never `Command::new("systemctl")` or `command_available("systemctl")`.
- `session_manager_available()` / `NO_SESSION_MANAGER` — whether THIS host has a live-session environment manager, and the ONE wording for its absence; the plan, the apply's skip detail and `status`'s session row all answer from it.
- `reg_cmd()` / `REG_BIN_ENV` — the same for the Windows registry, shared by the session-env refresh and the `windowsRegistry` configurator.
- Keyed system configurators name their own seams beside their `tool_cmd` factories (`CFGD_GSETTINGS_BIN`, `CFGD_XFCONF_QUERY_BIN`, `CFGD_KREADCONFIG_BIN` / `CFGD_KWRITECONFIG_BIN`, `CFGD_DEFAULTS_BIN`; `windowsRegistry` reuses `CFGD_REG_BIN`). Drive them through `test_helpers::ToolShim`.
- `register_bootstrapped_path_dirs(dirs)` — make the PATH directories cfgd created THIS RUN visible to later resolutions; never `set_var("PATH", …)`, unsound once any thread is live.
- `bootstrapped_path_dirs()` — a snapshot of that registry.
- `path_with_dirs_prepended(current, dirs)` — the ONE composition of a PATH whose leading entries are `dirs`; `None` when the value would not change.
- `process_path_with_dirs_prepended(dirs)` — the same over THIS process's PATH; what every consumer outside cfgd-core calls.
- `restore_bootstrapped_path_dirs(dirs)` — test-only rewind; reach for it through `BootstrappedPathDirsGuard`.

## Git

- `git_cmd_safe(url, ssh_policy)` — a `Command` for git with `GIT_TERMINAL_PROMPT=0` and configurable host-key checking; required for anything that may touch a remote.
- `git_cmd_local()` — the LOCAL-only factory. Use instead of `Command::new("git")` for every local invocation.
- `refuse_option_like_revision(revision)` — the guard a git argv naming a REVISION carries in place of `--end-of-options`, which `git reset`/`checkout` REFUSE before git 2.43.7 (Ubuntu 24.04 ships 2.43.0); a revision argv carries a trailing `--` plus this refusal. `clone`/`fetch`/`ls-remote` keep the option. `no_revision_verb_argv_spells_end_of_options` walks every argv.
- `try_git_cmd(url, args, label, ssh_policy)` — run via `git_cmd_safe`, `true` on success; the CLI-first fallback before every git2 network operation, preventing SSH hangs.
- `resolve_repo_reference(value)` — the ONE resolution of a user-written repository reference, and what EVERY user-facing entry point calls; only the filesystem can say whether `acme/config` is a shorthand or a path.
- `expand_github_shorthand(value)` — the ONE `owner/repo` → GitHub URL expansion, answered from the STRING alone.
- `detect_default_branch(repo_dir)` — best-effort `origin/HEAD` then local `HEAD`.
- `detect_git_remote()` / `detect_git_head()` — the CWD repo's origin URL and HEAD SHA; use for artifact provenance instead of re-deriving.
- `git_ssh_credentials(url, username, allowed)` — the git2 credential callback (SSH agent + HTTPS helper).
- `fetch_git_source(git_src, cache_base, module_name, printer)` (`modules/git.rs`) — the ONE materialization of a module's git source; its two short-circuits must never be re-derived at a call site.
- `is_git_source(value)` (`modules/git.rs`) — the ONE git-URL predicate, pure and scheme-based, deliberately never probing the filesystem. `is_clonable_source` layers `--from`'s extra arms rather than widening it.

## Sigstore / cosign

- `cosign_cmd()` — the ONE cosign factory; consumers add the subcommand and flags.
- `oci::COSIGN_PREDICATE_TYPES` + `oci::attestation_type_name(uri)` — the ONE fold between the WIRE vocabulary and the FLAG vocabulary for attestation types; an unknown predicate is reported verbatim.
- `oci::artifact_facts(reference)` → `ArtifactFacts { platforms, attestations }` — the ONE registry visit answering both "what platforms" and "what attestations"; one read, so a status can never mix two visits.

## Hashing / versions

- `sha256_hex(data)` / `sha256_digest(data)` — SHA256 as hex, and as an OCI-style `sha256:<hex>`; use instead of inline `Sha256::digest`.
- `strip_sha256_prefix(s)` — strip `sha256:`; idempotent.
- `short_commit(commit)` — the 12-char display form of a commit id; every human surface naming a commit renders through it, persisted/`-o json` ids stay full. `init::source::commit_detail(commit)` owns the PROSE spelling (`at <commit>`); a named table column or kv key takes neither.
- `init::source::checkout_detail(dir)` (`crates/cfgd/src/cli/init/source.rs`) — the detail slot of every `init` row naming a config directory this run did not create: the checkout's own origin and revision, read off the repository rather than echoed from `--from`. `None` for a directory that is no checkout.
- `Sha256Stream` — the incremental form (`update`, `absorb_file`, `finish_hex` / `finish_digest`), for a digest over many inputs. The seam order IS the digest: never reorder a caller's parts.
- `parse_loose_version(s)` — 1/2/3-part version → semver `Version`.
- `version_satisfies(version, requirement)` — semver range check.

## Locks / reconcile

- `acquire_apply_lock(state_dir)` — the exclusive apply lock; returns an RAII guard.
- `acquire_source_lock(cache_dir, on_wait)` — the source-cache mutex, and the one lock that BLOCKS rather than refusing. Never the apply lock; `on_wait` announces through `printer.alert`. **Nothing in cfgd ever deletes a lock file.**
- `sources::discard_cached_checkout(cache_dir, name, printer)` — the ONE deletion of a source's cached checkout, holding the source lock; never a bare `remove_dir_all`. The one exception is a caller ALREADY holding that lock (`restore_accepted_checkout`), which removes inline and states why.
- `SourceManager::judge_cached_head(spec, source_dir, printer)` (`sources/mod.rs`) — the READ-path half of "nothing a subscription's demand refuses may stay composed": both refusals over a cached checkout settle here, un-composing the map entry while the checkout stays on disk for `cfgd sync` to repair.
- `sources::reset_checkout_to(repo_dir, commit)` + `SourceManager::restore_accepted_checkout(...)` — the ROLLBACK half of verify-then-publish: a refused new HEAD is reset back to the accepted commit (a first clone is removed outright), or one refused fetch strands the cache on the commit it rejected forever. `load_source_guarded` holds it for all four fetch/clone arms; the daemon's auto-pull is the second caller. The read-path recovery split (`cli::sync::resolution_failure_the_fetch_rejudges`, no wildcard) lives on those items' rustdoc; `a_source_refused_for_an_unsigned_head_syncs_once_a_signed_commit_lands` drives the whole recovery.
- `resolve_effective_reconcile(module, profile_chain, config)` / `EffectiveReconcile` — per-module reconcile settings resolved from patches, with no `Option`s left.

## Config inputs (what a derivation READ)

- `record_config_input(path)` — the ONE report that a file or listing was consulted while deriving typed config, called from the READ sites so a caller never GUESSES the file set.
- `ConfigInputRecorder::start()` / `.finish()` — the RAII frame; frames nest, so an inner recorder does not steal an outer one's entries.
- `ConfigInputs::unchanged()` — re-stat every entry. An EMPTY set answers `false`.
- `daemon::tick_cache::TickCache` — the daemon's holder built on the above, and the shape a second long-lived holder reaches for rather than minting a new fingerprint scheme.

## Encryption

- `is_file_encrypted(path, backend)` — sops (`sops.mac` + `lastmodified`) or age header detection.

## Snapshot normalizers

Plain `cfgd_core::*` exports from `util/paths.rs` that make a captured render host-stable. Snapshot tests reach them through `normalize_for_snapshot`; call one directly only for a single fold.

- `normalize_for_snapshot(captured, &[(path, label)])` — the composed entry point: `\`→`/`, CRLF→LF, and each path substituted with its label.
- `normalize_cfgd_version(s, version)` — substitute the EXACT running version, so a wrong version still fails to match.
- `normalize_snapshot_durations(raw)` — replace every ` (N.Ns)` elapsed suffix with ` (XXs)`; never re-implement the scan.

## Test guards

Reached via `cfgd_core::test_helpers::*`, gated behind the `test-helpers` Cargo feature. Pair every env-var consumer with `serial_test::serial`; which exclusion each TTL guard needs is in that module's doc.

- `BootstrappedPathDirsGuard::capture()` / `::capture_and_clear()` — RAII snapshot+restore of the bootstrapped-PATH registry, REQUIRED in any fixture driving a bootstrap; emptying `PATH` is not sufficient for a "not found" branch.
- `path_env_read_guard()` / `path_env_mutation_guard()` — the gate over the process-global `PATH`. A mutating test takes the WRITE guard, declared before its `EnvVarGuard`; never spawn while holding the write guard.
- `await_queued_path_writer(timeout)` — blocks until a writer is queued; the observable a concurrency test needs instead of a sleep.
- `await_blocking_source_acquire(timeout)` — the same observable for the source lock's blocking arm. Wait on this, never on `on_wait`, which fires BEFORE the acquire.
- `CommandPathMemoTtlGuard::{never_expires, always_expired, pinned}` — RAII pin of the `command_path` TTL; needs no serialization.
- `AvailableVersionMemoTtlGuard::…` — the same for the available-version ceiling; pair with `#[serial_test::serial(available_version_memo)]`.
- `AvailabilityMemoTtlGuard::…` — the same for the provider-availability sweep; pair with the UNNAMED `#[serial_test::serial]`.
- `ConfigReuseMaxAgeGuard::…` / `ModuleReuseTtlGuard::…` — the tick cache's two reuse ceilings; pair with `#[serial_test::serial(tick_cache_reuse)]`.
- `GitRefreshWindowGuard::…` — the module git-cache refresh window; the pin SERIALIZES ITSELF.
- `measured_in_a_stable_generation(measure)` — run `measure` in a window where nothing else moved the resolution generation. REQUIRED by every memo-hit claim; the closure must be re-runnable.
- `captured_text(&buf)` — the ONE read of a capture buffer, ANSI-stripped; a negative `!contains` goes VACUOUS the moment styling is on.
- `Printer::for_test_split_streams(verbosity)` — the split-stream capture, the one constructor that can state a stdout-purity claim directly.
- `Printer::for_test_with_theme_colored(theme, verbosity)` — the ONE capture whose buffer carries ANSI, for a test whose subject IS the escapes; to assert the colour DECISION, call `output::printer::colors_must_be_disabled(&format)` and render nothing.
- **The three live-region capture constructors** — they differ only in where indicatif draws, so the wrong one answers a different question SILENTLY: `for_test_live_scrollback()` sees only committed output (commit order/exactly-once); `for_test_with_live_bars()` holds every paint in order (interleaving, draw order); `for_test_live_terminal(rows, cols)` executes cursor moves on an emulated screen and is the ONLY capture that sees stranded paints or answers `wrap_columns()` with a width — every width claim pins here.
- `spinner::start_spinner_animation(bar)` (`output/spinner.rs`) — the ONE way a bar starts animating, and the LAST call of any spinner setup: a tick started before the message is in paints a lone glyph on an empty line. Its `debug_assert` on a non-empty message is the seam a test pins.
- `test_printer()` — a bare Quiet `Printer` for a fixture that asserts nothing about output. NEVER `Printer::new`, which inherits the invoking terminal and hangs under a pty.
- `EnvVarGuard::set(key, value)` / `::unset(key)` — RAII env-var save/restore, restored even on panic.
- `with_test_env_var(var, value, f)` — the scoped-closure form.
- `spawn_blocking_with_test_home(f)` — `spawn_blocking` re-installing the caller's test-home thread-local; REQUIRED for every blocking dispatch whose closure may resolve `~`.
- `ConcurrencyWitness` + `MockPackageManager::with_concurrency_witness(w)` — proof that two lanes really overlapped, so a concurrency test asserts a peak, never a wall-clock bound.
- `ProbePath::containing(&[names])` — a `PATH` of one temp dir holding exactly the named executables (Unix-only). Assert the negative under an empty `PATH` and the positive here.
- `CosignTestShim::install()` / `::builder()...install()` — the fake-cosign shim, restoring the prior seam on drop.
- `StateStore::source_conflict_count()` (`state/sources.rs`, `cfg(feature = "test-helpers")`) — the count of persisted conflict rows, for pins that a composition ran once.
- `freeze_last_scan_at(&StateStore, timestamp)` — pins the recorded scan stamp and then REFUSES every later write, so a `cfgd`-crate test can drive the refused-write branch.

## Upgrade

- `cleanup_old_binary()` (`upgrade.rs`) — remove the `.exe.old` left by the Windows rename-dance self-upgrade; no-op on Unix, called from `main.rs` on startup.

## What NOT to do

- Don't create new utility files outside `cfgd-core/src/util/`. Shared functions go in the existing topic file that matches the helper's domain.
- Don't add the same helper as a sibling of an existing topic file. Pick the existing topic.
- Don't create a brand-new topic file unless the helper genuinely doesn't fit any existing one — three string-validation functions don't justify a new file when `strings.rs` exists.
- Don't duplicate a function that already exists. Search this catalog first.
- Don't create local timestamp/hash/command-check wrappers — use the shared ones above.
- Don't restate an item's rationale here. This file says WHAT and WHEN; the item's rustdoc says WHY, and a second copy drifts.
