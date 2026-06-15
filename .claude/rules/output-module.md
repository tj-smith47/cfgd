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

Forbidden outside the `output/` module itself:
- `println!`, `eprintln!`, `print!`, `eprint!`
- `console::*` direct use
- `indicatif::ProgressBar::new` or `MultiProgress::new` directly
- `log::*` macros — use `tracing::*` instead
- The following method names are reserved-banned (the audit gate in `.claude/scripts/audit.sh` rejects them outside `output/` itself): `success`, `warning`, `info`, `error`, `header`, `subheader`, `key_value`, `newline`, `plan_phase`, `stdout_line`.

See Hard Rule #1 in `hard-rules.md`.

## Structured-output coverage (cmd_* → has_data_payload?)

Every `cmd_*` function in `crates/cfgd/src/cli/` must appear in this
table. The audit greps for `cmd_*` declarations and fails if any are
missing from the table.

| Command                      | has_data_payload? | Why / Why not                                      |
|------------------------------|-------------------|----------------------------------------------------|
| alias_list                   | yes               | alias inventory                                    |
| apply                        | yes               | apply-result records consumed by CI                |
| checkin                      | yes               | machine identity exposed to gateway                |
| clusterconfigpolicy_validate | yes               | validation result consumed by scripts/CI           |
| compliance_diff              | yes               | drift reporting                                    |
| compliance_export            | yes               | compliance data exported to scripts                |
| compliance_history           | yes               | drift history queried by scripts                   |
| compliance_snapshot          | yes               | snapshot consumed by scripts                       |
| config_edit                  | no                | opens $EDITOR; no data output                      |
| config_get                   | yes               | key/value queried by scripts                       |
| configpolicy_validate        | yes               | validation result consumed by scripts/CI           |
| config_set                   | yes               | mutation records                                   |
| config_show                  | yes               | inspector consumed by scripts                      |
| config_unset                 | yes               | mutation records                                   |
| daemon                       | no                | dispatcher only                                    |
| daemon_install               | no                | one-shot setup; no scripting consumer              |
| daemon_service               | no                | internal service registration; no scripting consumer |
| daemon_status                | yes               | daemon health queried by scripts                   |
| daemon_uninstall             | no                | one-shot teardown; no scripting consumer           |
| debug                        | no                | kubectl plugin dev-tooling                         |
| decide                       | no                | interactive flow                                   |
| deploy                       | yes               | image-volume pin rewrites consumed by CI           |
| diff                         | yes               | drift reporting                                    |
| diff_module                  | yes               | per-module drift reporting                         |
| doctor                       | no                | dev-tooling                                        |
| enroll                       | yes               | machine identity exposed to gateway                |
| exec                         | no                | kubectl plugin; raw command execution              |
| explain                      | no                | dev-tooling                                        |
| generate                     | yes               | generated module metadata                          |
| generate_scan_only           | yes               | scan results consumed by scripts                   |
| image_pack                   | yes               | packed-image artifact + digest records             |
| init                         | no                | one-shot setup; no scripting consumer              |
| inject                       | no                | kubectl plugin; pod mutation                       |
| log                          | no                | already a streaming log surface                    |
| log_show_output              | no                | streaming log display helper                       |
| machineconfig_validate       | yes               | validation result consumed by scripts/CI           |
| module_add_from_registry     | yes               | add-result records                                 |
| module_add_remote            | yes               | add-result records                                 |
| module_build                 | yes               | build artifact records                             |
| module_create                | yes               | new module metadata                                |
| module_delete                | yes               | deletion records                                   |
| module_edit                  | no                | opens $EDITOR; no data output                      |
| module_export                | yes               | export artifact metadata                           |
| module_keys_generate         | yes               | key pair paths                                     |
| module_keys_list             | yes               | key inventory                                      |
| module_keys_rotate           | yes               | rotation records                                   |
| module_list                  | yes               | module inventory                                   |
| module_pull                  | yes               | pull result records                                |
| module_push                  | yes               | push artifact records                              |
| module_registry_add          | yes               | registry configuration records                     |
| module_registry_list         | yes               | registry inventory                                 |
| module_registry_remove       | yes               | removal records                                    |
| module_registry_rename       | yes               | rename records                                     |
| module_search                | yes               | search results consumed by scripts                 |
| module_show                  | yes               | introspection                                      |
| module_update_local          | yes               | update records                                     |
| module_upgrade               | yes               | upgrade result records                             |
| module_validate              | yes               | validation result consumed by scripts/CI           |
| paths                        | yes               | resolved directory roots consumed by scripts       |
| plan                         | yes               | plan output consumed by CI                         |
| profile_create               | yes               | new profile metadata                               |
| profile_delete               | yes               | deletion records                                   |
| profile_edit                 | no                | opens $EDITOR; no data output                      |
| profile_list                 | yes               | profile inventory                                  |
| profile_show                 | yes               | introspection                                      |
| profile_switch               | yes               | switch records                                     |
| profile_update               | yes               | update records                                     |
| profile_validate             | yes               | validation result consumed by scripts/CI           |
| pull                         | yes               | pull result records                                |
| rollback                     | yes               | rollback result records                            |
| secret_decrypt               | no                | plaintext via data_line; not a structured payload  |
| secret_edit                  | no                | opens $EDITOR; no data output                      |
| secret_encrypt               | yes               | encryption result records                          |
| secret_init                  | yes               | backend configuration records                      |
| source_add                   | yes               | add-result records                                 |
| source_create                | yes               | new source metadata                                |
| source_edit                  | no                | opens $EDITOR; no data output                      |
| source_list                  | yes               | source inventory                                   |
| source_override              | yes               | override records                                   |
| source_priority              | yes               | priority change records                            |
| source_remove                | yes               | removal records                                    |
| source_replace               | yes               | replacement records                                |
| source_show                  | yes               | introspection                                      |
| source_update                | yes               | update result records                              |
| source_validate              | yes               | validation result consumed by scripts/CI           |
| status                       | yes               | drift + last-apply queried by scripts              |
| status_module                | yes               | per-module status queried by scripts               |
| sync                         | yes               | sync result records                                |
| upgrade                      | yes               | upgrade result records                             |
| verify                       | no                | dev-loop only                                      |
| version                      | yes               | version info queried by scripts                    |
| workflow_generate            | yes               | generated workflow metadata                        |

Error-path `Doc`s also carry `with_data` with a `{"error": "...", "name": "...", ...}` payload so structured consumers see a consistent shape on failure.
